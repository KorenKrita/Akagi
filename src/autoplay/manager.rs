//! Autoplay manager: subscribes to bot decisions + mjai events,
//! translates them into UI clicks dispatched via CDP.
//!
//! Lifecycle:
//! - Spawned by `crate::lib::run` when `cfg.autoplay.enabled = true`.
//! - One long-lived `tokio::select!` loop over `BotResponseBus` and
//!   `MjaiBus`. Bot responses drive clicks; mjai events update local
//!   per-game tracking state (`last_kawa_tile`, `last_self_tsumo`,
//!   `self_riichi_accepted`, `reach_state`).
//!
//! Failure modes are silent-by-design: if the page handle is missing
//! (chromium backend not running) or the canvas-rect query fails, the
//! manager logs a warning and skips the click. The bot pipeline is
//! untouched; the user can still play the round manually.

use crate::autoplay::cdp_input::{
    dispatch_click, dispatch_mouse_move, dispatch_ws_binary, evaluate_canvas_rect,
};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::majsoul::MajsoulAutoplay;
use crate::autoplay::platform::{ActionContext, PlatformAutoplay, ReachState, Step};
use crate::bot::BotResponse;
use crate::bridge::majsoul::tile::{compare_pai, mjai_to_ms};
use crate::bridge::BuildHints;
use crate::config::{AppConfig, MajsoulAutoplayMode};
use crate::event_bus::{BotResponseBus, MjaiBus};
use crate::game_state::snapshot::Phase;
use crate::game_state::tracker::GameTracker;
use crate::schema::MjaiEvent;
use riichienv_core::action::{Action, ActionType};
use riichienv_core::parser::tid_to_mjai;
use riichienv_core::state::legal_actions::GameStateLegalActions;
use riichienv_core::state_3p::legal_actions::GameState3PLegalActions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast::error::RecvError, Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// How long before a cached `CanvasRect` is treated as stale and re-queried.
const CANVAS_RECT_TTL: Duration = Duration::from_secs(30);
const ACTION_RETRY_AFTER: Duration = Duration::from_secs(1);

pub struct AutoplayManager {
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    mjai_bus: MjaiBus,
    platform: Arc<dyn PlatformAutoplay>,
    state: ManagerState,
    /// User Lua delay policy (hot-reloaded from disk; see
    /// `autoplay::delay::script`).
    delay_script: crate::autoplay::delay::ScriptHost,
    /// Directory holding the loaded config file; the script lives at
    /// `<config_dir>/delay.lua`.
    config_dir: std::path::PathBuf,
}

#[derive(Default)]
struct ManagerState {
    last_kawa_tile: Option<String>,
    last_self_tsumo: Option<String>,
    self_riichi_accepted: bool,
    reach_state: ReachState,
    canvas_rect_at: Option<Instant>,
    /// Cached seat index for our player. Captured directly from
    /// `StartGame { id }` and kept across kyoku resets. Avoids try_lock
    /// failures in the synchronous mjai event handler causing missed
    /// tsumo/dahai updates, and is available from the very first event
    /// rather than waiting for the first successful `handle_bot_response`.
    cached_our_seat: Option<u8>,
}

impl AutoplayManager {
    pub fn new(
        cfg: Arc<RwLock<AppConfig>>,
        ctx: Arc<AutoplayContext>,
        tracker: Arc<Mutex<GameTracker>>,
        mjai_bus: MjaiBus,
        config_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            cfg,
            ctx,
            tracker,
            mjai_bus,
            // Only Majsoul is wired up today; future Tenhou impl swaps
            // here based on config.platform.kind at run start.
            platform: Arc::new(MajsoulAutoplay::new()),
            state: ManagerState::default(),
            delay_script: crate::autoplay::delay::ScriptHost::default(),
            config_dir,
        }
    }

    /// Run forever. Returns `Err` only on bus closure (process exit).
    pub async fn run(mut self, response_bus: BotResponseBus) -> anyhow::Result<()> {
        let mut bot_rx = response_bus.subscribe();
        let mut mjai_rx = self.mjai_bus.subscribe();
        info!("autoplay manager started");

        loop {
            tokio::select! {
                msg = bot_rx.recv() => match msg {
                    Ok(resp) => self.handle_bot_response(resp).await,
                    Err(RecvError::Lagged(n)) => warn!("autoplay: bot bus lagged {n}"),
                    Err(RecvError::Closed) => {
                        info!("autoplay: bot bus closed; exiting");
                        return Ok(());
                    }
                },
                msg = mjai_rx.recv() => match msg {
                    Ok(ev) => self.handle_mjai_event(&ev),
                    Err(RecvError::Lagged(n)) => warn!("autoplay: mjai bus lagged {n}"),
                    Err(RecvError::Closed) => {
                        info!("autoplay: mjai bus closed; exiting");
                        return Ok(());
                    }
                },
            }
        }
    }

    async fn handle_bot_response(&mut self, resp: BotResponse) {
        // Re-read config every iteration so `cfg.autoplay.enabled` can be
        // toggled at runtime via the Settings UI without restarting.
        let cfg_guard = self.cfg.read().await;
        if !cfg_guard.autoplay.enabled {
            return;
        }
        let cfg = cfg_guard.autoplay.majsoul.clone();
        let delay_cfg = cfg_guard.autoplay.delay.clone();
        drop(cfg_guard);

        // Snapshot the server time budget for the current decision window
        // (written by the Majsoul bridge; None off-Majsoul or pre-game) and
        // normalize the bot's confidence metadata. Both feed the delay
        // model — neither can alter the chosen action. `opened_at` is kept
        // as the window's identity for the post-sleep staleness check.
        let planned_budget = self.ctx.time_budget.read().ok().and_then(|g| *g);
        let budget = planned_budget.map(|b| crate::autoplay::delay::BudgetSnapshot {
            fixed_ms: b.fixed_ms,
            add_ms: b.add_ms,
            elapsed_ms: b.elapsed_ms(),
        });
        let probs = crate::autoplay::delay::probs::normalize_meta(resp.meta.as_ref());

        // The delay script lives at a fixed path next to the config file.
        // In Lua mode it is generated from the bundled default when
        // missing, then hot-reloaded on change (cheap mtime stat). In
        // legacy mode the script is dropped entirely.
        let lua_mode = delay_cfg.mode == crate::config::DelayMode::Lua;
        let script_path = self.config_dir.join("delay.lua");
        if lua_mode {
            self.delay_script.ensure_default(&script_path);
        }
        self.delay_script.maybe_reload(&script_path, lua_mode);

        // Pull our seat + legal actions from the live engine state. This
        // bracket releases the tracker mutex before we sleep/click.
        let (our_seat, legal_actions, snapshot, num_players) = {
            let tracker = self.tracker.lock().await;
            let our_seat = match tracker.our_seat() {
                Some(s) => s,
                None => return, // game hasn't started or no perspective tagged
            };
            // Keep cached_our_seat up to date for handle_mjai_event.
            self.state.cached_our_seat = Some(our_seat);
            let snapshot = match tracker.snapshot() {
                Some(s) => s,
                None => return,
            };
            let num_players = snapshot.num_players;
            let legal_actions: Vec<Action> = if num_players == 3 {
                tracker
                    .state_3p()
                    .map(|s| s._get_legal_actions_internal(our_seat))
                    .unwrap_or_default()
            } else {
                tracker
                    .state()
                    .map(|s| s._get_legal_actions_internal(our_seat))
                    .unwrap_or_default()
            };
            (our_seat, legal_actions, snapshot, num_players)
        };

        let action_retry_guard = retry_guard_for_action(&resp.action, our_seat, &snapshot);

        let use_packet = matches!(
            cfg.mode,
            MajsoulAutoplayMode::Packet | MajsoulAutoplayMode::PacketWithClickFallback
        );
        let allow_click = matches!(
            cfg.mode,
            MajsoulAutoplayMode::Click | MajsoulAutoplayMode::PacketWithClickFallback
        );

        let packet_action = normalize_packet_action(
            &resp.action,
            our_seat,
            &snapshot,
            self.state.last_self_tsumo.as_deref(),
        );
        let mut packet_click_fallback_reason: Option<String> = None;

        if use_packet && packet_action_allowed(&packet_action, our_seat, &snapshot, &legal_actions)
        {
            let packet_hints = packet_build_hints(
                &packet_action,
                our_seat,
                &snapshot,
                &legal_actions,
                self.state.last_self_tsumo.as_deref(),
            );
            match self.try_packet_action(&packet_action, &packet_hints).await {
                PacketDispatch::Sent => {
                    if let Some(guard) = action_retry_guard.as_ref() {
                        tokio::time::sleep(ACTION_RETRY_AFTER).await;
                        if self.action_still_pending(guard).await {
                            debug!(
                                "autoplay: packet action {:?} still pending after {:?}; retrying",
                                packet_action, ACTION_RETRY_AFTER
                            );
                            let _ = self.try_packet_action(&packet_action, &packet_hints).await;
                            if self.action_still_pending(guard).await {
                                error!(
                                "autoplay: packet action {:?} did not advance game state after retry; phase={:?} current_player={} our_seat={} legal_actions=[{}] player={} before={:?} hints={:?}",
                                packet_action,
                                snapshot.phase,
                                snapshot.current_player,
                                our_seat,
                                legal_actions_summary(&legal_actions),
                                player_snapshot_summary(&snapshot, our_seat),
                                guard.before,
                                packet_hints
                            );
                                packet_click_fallback_reason = Some(format!(
                                    "sent packet did not advance game state after retry; hints={packet_hints:?}"
                                ));
                            } else {
                                self.after_action_sent(&resp.action, our_seat, false);
                                return;
                            }
                        } else {
                            self.after_action_sent(&resp.action, our_seat, false);
                            return;
                        }
                    } else {
                        self.after_action_sent(&resp.action, our_seat, false);
                        return;
                    }
                }
                reason => {
                    error!(
                        "autoplay: packet action {:?} not sent: {:?}; phase={:?} current_player={} our_seat={} legal_actions=[{}] player={} hints={:?}",
                        packet_action,
                        reason,
                        snapshot.phase,
                        snapshot.current_player,
                        our_seat,
                        legal_actions_summary(&legal_actions),
                        player_snapshot_summary(&snapshot, our_seat),
                        packet_hints
                    );
                    packet_click_fallback_reason = Some(format!(
                        "packet was not sent: {reason:?}; hints={packet_hints:?}"
                    ));
                }
            }
        } else if use_packet {
            match packet_action {
                MjaiEvent::None => {
                    debug!("autoplay: suppressed packet None outside a visible response window");
                }
                MjaiEvent::Dahai { .. } | MjaiEvent::Kita { .. } => {
                    if snapshot.current_player == our_seat {
                        error!(
                            "autoplay: suppressed currently illegal packet action {:?}; phase={:?} current_player={} our_seat={} legal_actions=[{}]",
                            packet_action,
                            snapshot.phase,
                            snapshot.current_player,
                            our_seat,
                            legal_actions_summary(&legal_actions)
                        );
                    } else {
                        debug!(
                            "autoplay: ignored stale packet action {:?}; phase={:?} current_player={} our_seat={} legal_actions=[{}]",
                            packet_action,
                            snapshot.phase,
                            snapshot.current_player,
                            our_seat,
                            legal_actions_summary(&legal_actions)
                        );
                    }
                    return;
                }
                _ => {}
            }
        }

        if !allow_click {
            debug!("autoplay: packet-only mode did not send {:?}", resp.action);
            return;
        }

        let action_ctx = ActionContext {
            action: &resp.action,
            snapshot: &snapshot,
            legal_actions: &legal_actions,
            our_seat,
            last_kawa_tile: self.state.last_kawa_tile.as_deref(),
            last_self_tsumo: self.state.last_self_tsumo.as_deref(),
            self_riichi_accepted: self.state.self_riichi_accepted,
            reach_state: self.state.reach_state,
            num_players,
            cfg: &cfg,
            delay_cfg,
            budget,
            probs,
            delay_script: self.delay_script.script(),
        };

        let plan = self.platform.plan(&action_ctx);
        if plan.steps.is_empty() && !plan.inject_reach_for_followup {
            return;
        }

        debug!(
            "autoplay: action={:?} steps={} inject_reach={} await_riichi_dahai={}",
            resp.action,
            plan.steps.len(),
            plan.inject_reach_for_followup,
            plan.awaiting_riichi_dahai
        );

        if let Some(reason) = &packet_click_fallback_reason {
            error!(
                "autoplay: falling back to click for {:?} after packet failure: {}; phase={:?} current_player={} our_seat={} legal_actions=[{}] player={}",
                resp.action,
                reason,
                snapshot.phase,
                snapshot.current_player,
                our_seat,
                legal_actions_summary(&legal_actions),
                player_snapshot_summary(&snapshot, our_seat)
            );
        }

        // Resolve a canvas rect (cache + TTL). If we can't, drop the
        // click - the page handle isn't ready yet (e.g. user still on
        // the lobby), or the chromium backend isn't running at all.
        let rect = match self.canvas_rect_resolve().await {
            Some(r) => r,
            None => {
                warn!(
                    "autoplay: no canvas rect - skipping click for {:?}",
                    resp.action
                );
                return;
            }
        };

        if !self
            .execute_steps(
                &plan.steps,
                rect,
                &cfg,
                true,
                false,
                planned_budget,
                &resp.action,
            )
            .await
        {
            return;
        }

        if let Some(guard) = action_retry_guard {
            tokio::time::sleep(ACTION_RETRY_AFTER).await;
            if self.action_still_pending(&guard).await {
                debug!(
                    "autoplay: action {:?} still pending after {:?}; retrying click",
                    resp.action, ACTION_RETRY_AFTER
                );
                let rect = self.canvas_rect_resolve().await.unwrap_or(rect);
                let click_steps: Vec<Step> = plan
                    .steps
                    .iter()
                    .filter(|s| matches!(s, Step::Click { .. }))
                    .cloned()
                    .collect();
                let _ = self
                    .execute_steps(
                        &click_steps,
                        rect,
                        &cfg,
                        false,
                        true,
                        planned_budget,
                        &resp.action,
                    )
                    .await;
            }
        }

        // Path-B side effect: inject synthetic Reach so the bot will
        // emit the riichi-declaring dahai we need to click next.
        self.after_action_sent(&resp.action, our_seat, plan.inject_reach_for_followup);
    }

    async fn try_packet_action(&self, action: &MjaiEvent, hints: &BuildHints) -> PacketDispatch {
        let bridge = { self.ctx.packet_bridge.read().await.clone() };
        let Some(bridge) = bridge else {
            return PacketDispatch::NoPacketBridge;
        };
        let frame = {
            let mut bridge = bridge.lock().expect("packet bridge mutex poisoned");
            bridge.build_with_hints(action, hints)
        };
        let Some(frame) = frame else {
            return PacketDispatch::BuildReturnedNone;
        };
        let page = { self.ctx.page.read().await.clone() };
        let Some(page) = page else {
            return PacketDispatch::NoPage;
        };
        let target_url = { self.ctx.packet_ws_url.read().await.clone() };
        let Some(target_url) = target_url else {
            return PacketDispatch::NoPacketWsUrl;
        };
        self.ctx.mark_injected_ws_frame(&frame).await;
        match dispatch_ws_binary(&page, &target_url, &frame).await {
            Ok(true) => {
                info!("autoplay: sent packet for {:?}", action);
                PacketDispatch::Sent
            }
            Ok(false) => {
                debug!(
                    "autoplay: packet WebSocket hook has no open socket for {:?}",
                    action
                );
                PacketDispatch::HookNoOpenSocket
            }
            Err(e) => {
                warn!("autoplay: packet dispatch failed for {:?}: {e:#}", action);
                PacketDispatch::CdpError
            }
        }
    }

    fn after_action_sent(
        &mut self,
        action: &MjaiEvent,
        our_seat: u8,
        inject_reach_for_followup: bool,
    ) {
        if inject_reach_for_followup {
            self.state.reach_state = ReachState::AwaitingDahai;
            let synthetic = MjaiEvent::Reach {
                actor: our_seat,
                pai: None,
            };
            if let Err(e) = self.mjai_bus.send(synthetic) {
                debug!("autoplay: synthetic Reach send had no subscribers: {e:?}");
            } else {
                info!("autoplay: injected synthetic Reach (Path B) for seat {our_seat}");
            }
        }

        if matches!(self.state.reach_state, ReachState::AwaitingDahai) {
            if let MjaiEvent::Dahai { actor, .. } = action {
                if *actor == our_seat {
                    self.state.reach_state = ReachState::Idle;
                }
            }
        }
    }

    async fn execute_steps(
        &mut self,
        steps: &[Step],
        rect: CanvasRect,
        cfg: &crate::config::MajsoulAutoplayConfig,
        include_sleeps: bool,
        force_hover_refresh: bool,
        planned_budget: Option<crate::autoplay::budget::TimeBudget>,
        action: &MjaiEvent,
    ) -> bool {
        let mut window_checked = false;
        for step in steps {
            match step {
                Step::Sleep { duration_ms } => {
                    if include_sleeps {
                        tokio::time::sleep(Duration::from_millis(*duration_ms as u64)).await;
                    }
                }
                Step::Click { x_norm, y_norm } => {
                    // The decision window can close while we sleep: a
                    // higher-priority claimant (ron over our chi window)
                    // resolves it early, and the *next* window's buttons
                    // render at the same coordinates — a stale click
                    // would press a live button of the wrong decision.
                    // The bridge rewrites the budget slot exactly when
                    // that happens, so the slot still holding the window
                    // we planned against is the cheap validity check.
                    // Checked once, before the first click: later steps
                    // of one plan run inside our own action's window.
                    // With no budget tracked (off-Majsoul) there is no
                    // signal — behaviour is unchanged there.
                    if !window_checked {
                        window_checked = true;
                        if planned_budget.is_some() {
                            let current = self.ctx.time_budget.read().ok().and_then(|g| *g);
                            if current.map(|b| b.opened_at) != planned_budget.map(|b| b.opened_at) {
                                warn!(
                                    "autoplay: decision window closed mid-delay — dropping stale click for {:?}",
                                    action
                                );
                                return false;
                            }
                        }
                    }
                    let (px, py) = rect.pixel(*x_norm, *y_norm);
                    if !rect.contains(px, py) {
                        warn!(
                            "autoplay: click ({px},{py}) outside canvas rect {:?}; skipping",
                            rect
                        );
                        continue;
                    }
                    // Need to re-acquire the page handle on each click;
                    // it may have been replaced (tab reload) between
                    // successive clicks within one action.
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle - aborting click sequence");
                        return false;
                    };
                    if force_hover_refresh {
                        let reset_x = (px - 24.0).max(rect.x);
                        let reset_y = (py - 24.0).max(rect.y);
                        if let Err(e) = dispatch_mouse_move(page, reset_x, reset_y).await {
                            warn!("autoplay: retry move-away failed: {e:#}");
                            return false;
                        }
                    }
                    if let Err(e) =
                        dispatch_click(page, px, py, cfg.hover_delay_ms, cfg.click_hold_ms).await
                    {
                        warn!("autoplay: dispatch_click failed: {e:#}");
                        return false;
                    }
                    drop(page_guard);
                }
            }
        }
        true
    }

    async fn action_still_pending(&self, guard: &ActionRetryGuard) -> bool {
        let tracker = self.tracker.lock().await;
        let Some(snapshot) = tracker.snapshot() else {
            return false;
        };
        snapshot_fingerprint(&snapshot, guard.our_seat).as_ref() == Some(&guard.before)
    }

    fn handle_mjai_event(&mut self, ev: &MjaiEvent) {
        match ev {
            MjaiEvent::StartGame { id, .. } => {
                // Capture our seat directly from the StartGame event rather
                // than going through the tracker. This avoids the try_lock
                // race entirely and makes cached_our_seat available from the
                // very first event of the game.
                let seat = *id;
                self.state = ManagerState::default();
                self.state.cached_our_seat = seat;
            }
            MjaiEvent::EndGame => {
                self.state = ManagerState::default();
            }
            MjaiEvent::StartKyoku { .. } | MjaiEvent::EndKyoku => {
                // Per-kyoku reset: keep last seen rect cache and cached seat,
                // drop everything else. Keep last_kawa_tile as None so
                // push_random_pre_delay uses the max delay (opening-hand guard).
                let canvas_at = self.state.canvas_rect_at;
                let cached_seat = self.state.cached_our_seat;
                self.state = ManagerState::default();
                self.state.canvas_rect_at = canvas_at;
                self.state.cached_our_seat = cached_seat;
            }
            MjaiEvent::Tsumo { actor, pai } => {
                if let Some(seat) = self.our_seat_cached() {
                    if *actor == seat {
                        self.state.last_self_tsumo = Some(pai.clone());
                    }
                }
            }
            MjaiEvent::Dahai { actor, pai, .. } => {
                self.state.last_kawa_tile = Some(pai.clone());
                if let Some(seat) = self.our_seat_cached() {
                    if *actor == seat {
                        self.state.last_self_tsumo = None;
                    }
                }
            }
            MjaiEvent::ReachAccepted { actor } => {
                if let Some(seat) = self.our_seat_cached() {
                    if *actor == seat {
                        self.state.self_riichi_accepted = true;
                    }
                }
            }
            MjaiEvent::Chi { actor, .. }
            | MjaiEvent::Pon { actor, .. }
            | MjaiEvent::Daiminkan { actor, .. }
            | MjaiEvent::Ankan { actor, .. }
            | MjaiEvent::Kakan { actor, .. }
            | MjaiEvent::Kita { actor, .. } => {
                if let Some(seat) = self.our_seat_cached() {
                    if *actor == seat {
                        self.state.last_self_tsumo = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Best-effort seat lookup. Uses the cached seat from `StartGame` first,
    /// falling back to try_lock on the tracker.
    fn our_seat_cached(&self) -> Option<u8> {
        self.state
            .cached_our_seat
            .or_else(|| self.tracker.try_lock().ok().and_then(|t| t.our_seat()))
    }

    async fn canvas_rect_resolve(&mut self) -> Option<CanvasRect> {
        let now = Instant::now();
        if let Some(at) = self.state.canvas_rect_at {
            if now.duration_since(at) < CANVAS_RECT_TTL {
                if let Some(r) = *self.ctx.canvas_rect.read().await {
                    return Some(r);
                }
            }
        }
        // Re-query.
        let page_guard = self.ctx.page.read().await;
        let page = page_guard.as_ref()?.clone();
        drop(page_guard);
        match evaluate_canvas_rect(&page).await {
            Ok(rect) => {
                *self.ctx.canvas_rect.write().await = Some(rect);
                self.state.canvas_rect_at = Some(now);
                Some(rect)
            }
            Err(e) => {
                debug!("autoplay: evaluate_canvas_rect failed: {e:#}");
                None
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActionRetryGuard {
    our_seat: u8,
    before: SnapshotFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFingerprint {
    phase: Phase,
    current_player: u8,
    is_done: bool,
    tehai_len: usize,
    river_len: usize,
    melds_len: usize,
    kita_len: usize,
    all_river_lens: Vec<usize>,
    all_meld_lens: Vec<usize>,
}

#[derive(Debug)]
enum PacketDispatch {
    Sent,
    NoPacketBridge,
    BuildReturnedNone,
    NoPage,
    NoPacketWsUrl,
    HookNoOpenSocket,
    CdpError,
}

fn retry_guard_for_action(
    action: &MjaiEvent,
    our_seat: u8,
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
) -> Option<ActionRetryGuard> {
    match action {
        MjaiEvent::Dahai { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Daiminkan { actor, .. }
        | MjaiEvent::Kakan { actor, .. }
        | MjaiEvent::Ankan { actor, .. }
        | MjaiEvent::Kita { actor, .. }
        | MjaiEvent::Hora { actor, .. }
            if *actor == our_seat =>
        {
            Some(ActionRetryGuard {
                our_seat,
                before: snapshot_fingerprint(snapshot, our_seat)?,
            })
        }
        MjaiEvent::Ryukyoku { .. } => Some(ActionRetryGuard {
            our_seat,
            before: snapshot_fingerprint(snapshot, our_seat)?,
        }),
        _ => None,
    }
}

fn snapshot_fingerprint(
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    our_seat: u8,
) -> Option<SnapshotFingerprint> {
    let player = snapshot.players.get(our_seat as usize)?;
    Some(SnapshotFingerprint {
        phase: snapshot.phase.clone(),
        current_player: snapshot.current_player,
        is_done: snapshot.is_done,
        tehai_len: player.tehai.len(),
        river_len: player.river.len(),
        melds_len: player.melds.len(),
        kita_len: player.kita_tiles.len(),
        all_river_lens: snapshot.players.iter().map(|p| p.river.len()).collect(),
        all_meld_lens: snapshot.players.iter().map(|p| p.melds.len()).collect(),
    })
}

fn normalize_packet_action(
    action: &MjaiEvent,
    our_seat: u8,
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    last_self_tsumo: Option<&str>,
) -> MjaiEvent {
    let MjaiEvent::Dahai { actor, pai, .. } = action else {
        return action.clone();
    };
    if *actor != our_seat {
        return action.clone();
    }
    let drawn = snapshot
        .players
        .get(our_seat as usize)
        .and_then(|p| p.drawn_tile.as_deref())
        .or(last_self_tsumo);
    let moqie = drawn == Some(pai.as_str());
    MjaiEvent::Dahai {
        actor: *actor,
        pai: pai.clone(),
        tsumogiri: moqie,
    }
}

fn packet_action_allowed(
    action: &MjaiEvent,
    our_seat: u8,
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    legal_actions: &[Action],
) -> bool {
    match action {
        MjaiEvent::None => {
            snapshot.phase == Phase::WaitResponse
                && legal_actions
                    .iter()
                    .any(|a| a.action_type == ActionType::Pass)
                && legal_actions.iter().any(|a| {
                    matches!(
                        a.action_type,
                        ActionType::Chi | ActionType::Pon | ActionType::Daiminkan | ActionType::Ron
                    )
                })
        }
        MjaiEvent::Dahai { actor, pai, .. } => {
            *actor == our_seat
                && snapshot.phase == Phase::WaitAct
                && snapshot.current_player == our_seat
                && legal_actions.iter().any(|a| {
                    a.action_type == ActionType::Discard
                        && a.tile.map(tid_to_mjai).as_deref() == Some(pai.as_str())
                })
        }
        MjaiEvent::Kita { actor, .. } => {
            *actor == our_seat
                && snapshot.phase == Phase::WaitAct
                && snapshot.current_player == our_seat
                && legal_actions
                    .iter()
                    .any(|a| a.action_type == ActionType::Kita)
        }
        MjaiEvent::Reach { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Daiminkan { actor, .. }
        | MjaiEvent::Ankan { actor, .. }
        | MjaiEvent::Kakan { actor, .. }
        | MjaiEvent::Hora { actor, .. } => *actor == our_seat,
        MjaiEvent::Ryukyoku { .. } => true,
        _ => false,
    }
}

fn legal_actions_summary(legal_actions: &[Action]) -> String {
    legal_actions
        .iter()
        .map(|a| {
            let tile = a.tile.map(tid_to_mjai).unwrap_or_else(|| "-".to_string());
            let consume = if a.consume_tiles.is_empty() {
                "-".to_string()
            } else {
                a.consume_tiles
                    .iter()
                    .copied()
                    .map(tid_to_mjai)
                    .collect::<Vec<_>>()
                    .join("/")
            };
            let actor = a
                .actor
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            format!(
                "{:?}:tile={tile}:consume={consume}:actor={actor}",
                a.action_type
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn packet_build_hints(
    action: &MjaiEvent,
    our_seat: u8,
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    legal_actions: &[Action],
    last_self_tsumo: Option<&str>,
) -> BuildHints {
    let mut hints = BuildHints {
        our_seat: Some(our_seat),
        ..BuildHints::default()
    };
    match action {
        MjaiEvent::Dahai { actor, pai, .. } if *actor == our_seat => {
            let drawn = snapshot
                .players
                .get(our_seat as usize)
                .and_then(|p| p.drawn_tile.as_deref())
                .or(last_self_tsumo);
            let moqie = drawn == Some(pai.as_str());
            hints.self_operation_index = packet_dahai_index(snapshot, our_seat, pai, drawn, moqie);
            hints.self_operation_tile = mjai_to_ms(pai).ok().map(str::to_string);
            hints.self_operation_moqie = Some(moqie);
        }
        MjaiEvent::Kakan { actor, pai, .. } if *actor == our_seat => {
            let index = legal_actions
                .iter()
                .filter(|a| a.action_type == ActionType::Kakan)
                .position(|a| a.tile.map(tid_to_mjai).as_deref() == Some(pai.as_str()))
                .unwrap_or(0) as u32;
            let drawn = snapshot
                .players
                .get(our_seat as usize)
                .and_then(|p| p.drawn_tile.as_deref())
                .or(last_self_tsumo);
            hints.self_operation_index = Some(index);
            hints.self_operation_tile = mjai_to_ms(pai).ok().map(str::to_string);
            hints.self_operation_moqie = Some(drawn == Some(pai.as_str()));
        }
        MjaiEvent::Kita { actor, .. } if *actor == our_seat => {
            let kita_index = legal_actions
                .iter()
                .filter(|a| a.action_type == ActionType::Kita)
                .position(|a| a.tile.map(|t| t / 4) == Some(30))
                .unwrap_or(0) as u32;
            let drawn_is_north = snapshot
                .players
                .get(our_seat as usize)
                .and_then(|p| p.drawn_tile.as_deref())
                .or(last_self_tsumo)
                == Some("N");
            hints.self_operation_index = Some(kita_index);
            hints.self_operation_tile = Some("4z".into());
            hints.self_operation_moqie = Some(drawn_is_north);
        }
        _ => {}
    }
    hints
}

fn packet_dahai_index(
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    our_seat: u8,
    pai: &str,
    drawn: Option<&str>,
    moqie: bool,
) -> Option<u32> {
    if moqie {
        return Some(13);
    }
    let player = snapshot.players.get(our_seat as usize)?;
    let mut sorted = player.tehai.clone();
    sorted.sort_by(|a, b| compare_pai(a, b));
    if let Some(drawn) = drawn {
        if let Some(pos) = sorted.iter().rposition(|tile| tile == drawn) {
            sorted.remove(pos);
        }
    }
    sorted
        .iter()
        .position(|tile| tile == pai)
        .map(|idx| idx as u32)
}

fn player_snapshot_summary(
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    our_seat: u8,
) -> String {
    let Some(player) = snapshot.players.get(our_seat as usize) else {
        return "<missing>".into();
    };
    format!(
        "tehai=[{}] drawn={:?} river_len={} melds_len={} kita=[{}]",
        player.tehai.join("/"),
        player.drawn_tile,
        player.river.len(),
        player.melds.len(),
        player.kita_tiles.join("/")
    )
}

/// Spawn point for the autoplay loop. Wired by `crate::lib::run` so the
/// `tauri::async_runtime` Tokio runtime is the host.
pub async fn run_autoplay_manager(
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    mjai_bus: MjaiBus,
    response_bus: BotResponseBus,
    config_dir: std::path::PathBuf,
) -> anyhow::Result<()> {
    AutoplayManager::new(cfg, ctx, tracker, mjai_bus, config_dir)
        .run(response_bus)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::context::AutoplayContext;
    use crate::event_bus::mjai_bus;
    use crate::game_state::tracker;

    /// Build a minimal `AutoplayManager` suitable for unit-testing
    /// `handle_mjai_event`. No CDP page, no config - just enough to
    /// exercise the mjai event handler without touching async resources.
    fn make_manager() -> AutoplayManager {
        let bus = mjai_bus();
        let tracker = tracker::new_handle();
        AutoplayManager::new(
            Arc::new(RwLock::new(AppConfig::default())),
            Arc::new(AutoplayContext::default()),
            tracker,
            bus,
            std::env::temp_dir(),
        )
    }

    /// Regression: `cached_our_seat` must be populated immediately when
    /// `StartGame` is received, before any bot response fires. Previously
    /// the seat was only cached inside `handle_bot_response`, so the first
    /// `Tsumo` event on the opening draw could arrive before the bot had
    /// responded and `last_self_tsumo` would be silently missed.
    #[test]
    fn start_game_sets_cached_seat_immediately() {
        let mut m = make_manager();

        // Before any event - no seat cached.
        assert!(m.state.cached_our_seat.is_none());

        // StartGame with id = Some(1) - seat must be cached right away.
        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(1),
            num_players: 4,
        });
        assert_eq!(
            m.state.cached_our_seat,
            Some(1),
            "seat must be cached from StartGame"
        );

        // A Tsumo by our seat (1) before any bot response should be recorded.
        m.handle_mjai_event(&MjaiEvent::Tsumo {
            actor: 1,
            pai: "3m".into(),
        });
        assert_eq!(
            m.state.last_self_tsumo.as_deref(),
            Some("3m"),
            "last_self_tsumo must be recorded even before first bot response"
        );
    }

    /// Seat is preserved across `StartKyoku` and `EndKyoku` resets so
    /// tsumo tracking continues to work from the first draw of each round.
    #[test]
    fn cached_seat_survives_kyoku_reset() {
        let mut m = make_manager();

        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(2),
            num_players: 4,
        });
        assert_eq!(m.state.cached_our_seat, Some(2));

        m.handle_mjai_event(&MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "1m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25_000; 4],
            tehais: vec![vec!["1m".into(); 13]; 4],
            num_players: 4,
        });
        assert_eq!(
            m.state.cached_our_seat,
            Some(2),
            "seat must survive StartKyoku reset"
        );

        m.handle_mjai_event(&MjaiEvent::EndKyoku);
        assert_eq!(
            m.state.cached_our_seat,
            Some(2),
            "seat must survive EndKyoku reset"
        );
    }

    /// Observer/replay mode: `StartGame` with `id: None` must not cache a
    /// stale seat from a previous game.
    #[test]
    fn start_game_without_id_clears_cached_seat() {
        let mut m = make_manager();

        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(0),
            num_players: 4,
        });
        assert_eq!(m.state.cached_our_seat, Some(0));

        // New game, observer mode - no seat.
        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: None,
            num_players: 4,
        });
        assert!(
            m.state.cached_our_seat.is_none(),
            "stale seat must be cleared"
        );
    }
}
