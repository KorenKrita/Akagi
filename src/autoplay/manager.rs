//! Autoplay manager: subscribes to bot decisions + mjai events,
//! translates them into UI clicks dispatched via CDP.
//!
//! Lifecycle:
//! - Spawned by `crate::lib::run` when `cfg.autoplay.enabled = true`.
//! - One long-lived `tokio::select!` loop over `BotResponseBus` and
//!   `MjaiBus`. Bot responses drive clicks; mjai events update local
//!   per-game tracking state (`last_kawa_tile`, `last_self_tsumo`,
//!   `self_riichi_accepted`).
//!
//! Failure modes are silent-by-design: if the page handle is missing
//! (chromium backend not running) or the canvas-rect query fails, the
//! manager logs a warning and skips the click. The bot pipeline is
//! untouched; the user can still play the round manually.

use crate::autoplay::cdp_input::{
    dispatch_click, dispatch_click_shaped, dispatch_mouse_move, dispatch_ws_binary,
    evaluate_canvas_rect,
};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::inject::InjectFrame;
use crate::autoplay::majsoul::{is_dealer_first_discard, MajsoulAutoplay};
use crate::autoplay::platform::{ActionContext, PlatformAutoplay, Step};
use crate::autoplay::riichi_city::RiichiCityAutoplay;
use crate::autoplay::tenhou::TenhouAutoplay;
use crate::autoplay::verify::InputTicket;
use crate::bot::BotResponse;
use crate::bridge::majsoul::tile::mjai_to_ms;
use crate::bridge::BuildHints;
use crate::config::{AppConfig, MajsoulAutoplayMode};
use crate::event_bus::{BotResponseBus, MjaiBus, NotifyBus};
use crate::game_state::snapshot::Phase;
use crate::game_state::tracker::GameTracker;
use crate::schema::MjaiEvent;
use chromiumoxide::page::Page;
use rand::Rng;
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
    /// For telling the user about a decision that came out wrong — see the
    /// riichi check in `handle_bot_response` and the dead-click reload.
    notify: NotifyBus,
    /// One implementation per supported platform, selected per decision from
    /// the live config so a platform switch takes effect without a restart.
    /// Majsoul synthesises clicks; Tenhou encodes a client frame.
    majsoul: MajsoulAutoplay,
    tenhou: TenhouAutoplay,
    /// Riichi City: no page to click — plans a protocol frame the proxy
    /// transmits (see `autoplay::inject`).
    riichi: RiichiCityAutoplay,
    state: ManagerState,
    /// User Lua delay policy (hot-reloaded from disk; see
    /// `autoplay::delay::script`).
    delay_script: crate::autoplay::delay::ScriptHost,
    /// Directory holding the loaded config file; the script lives at
    /// `<config_dir>/delay.lua`.
    config_dir: std::path::PathBuf,
    rematch_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct ManagerState {
    last_kawa_tile: Option<String>,
    last_self_tsumo: Option<String>,
    self_riichi_accepted: bool,
    canvas_rect_at: Option<Instant>,
    /// Decisions in a row where the client accepted nothing we pressed.
    /// Reset by any input the client does accept.
    dead_clicks: u32,
    /// Cached seat index for our player. Captured directly from
    /// `StartGame { id }` and kept across kyoku resets. Avoids try_lock
    /// failures in the synchronous mjai event handler causing missed
    /// tsumo/dahai updates, and is available from the very first event
    /// rather than waiting for the first successful `handle_bot_response`.
    cached_our_seat: Option<u8>,
    /// Tenhou: the decision window we last acted on, so the extra bot
    /// responses that arrive for the same one are dropped before they are
    /// planned rather than after.
    acted_window: Option<Instant>,
}

impl AutoplayManager {
    pub fn new(
        cfg: Arc<RwLock<AppConfig>>,
        ctx: Arc<AutoplayContext>,
        tracker: Arc<Mutex<GameTracker>>,
        mjai_bus: MjaiBus,
        notify: NotifyBus,
        config_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            cfg,
            ctx,
            tracker,
            mjai_bus,
            notify,
            majsoul: MajsoulAutoplay::new(),
            tenhou: TenhouAutoplay::new(),
            riichi: RiichiCityAutoplay::new(),
            state: ManagerState::default(),
            delay_script: crate::autoplay::delay::ScriptHost::default(),
            config_dir,
            rematch_task: None,
        }
    }

    /// Run forever. Returns `Err` only on bus closure (process exit).
    pub async fn run(mut self, response_bus: BotResponseBus) -> anyhow::Result<()> {
        let mut bot_rx = response_bus.subscribe();
        let mut mjai_rx = self.mjai_bus.subscribe();
        info!("autoplay manager started");
        // The round-advance watcher runs on its own subscription: at hand
        // end the plan loop is busy draining stale plans and the OK press
        // must not inherit that delay.
        let advance_cfg = self.cfg.clone();
        let advance_inject = self.ctx.inject.clone();
        let advance_bus = self.mjai_bus.clone();
        tauri::async_runtime::spawn(async move {
            crate::autoplay::riichi_city::round_advance::round_advance_watcher(
                advance_cfg,
                advance_inject,
                advance_bus,
            )
            .await;
        });

        self.start_lobby_watcher();

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
                    Ok(ev) => {
                        self.handle_mjai_event(&ev);
                    }
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
        let platform_kind = cfg_guard.platform.kind;
        drop(cfg_guard);

        // Tenhou's planner needs the bridge's hand at Tenhou tile-index
        // resolution; the slot stays empty on every other platform.
        let tenhou_state = self.ctx.tenhou_state.read().ok().and_then(|g| g.clone());
        // The window we plan against, kept as its own identity for the
        // post-delay staleness check (see `tenhou_window_moved`).
        let planned_window = tenhou_state.as_ref().and_then(|s| s.window);

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

        let action_ctx = ActionContext {
            action: &resp.action,
            snapshot: &snapshot,
            legal_actions: &legal_actions,
            our_seat,
            last_kawa_tile: self.state.last_kawa_tile.as_deref(),
            last_self_tsumo: self.state.last_self_tsumo.as_deref(),
            self_riichi_accepted: self.state.self_riichi_accepted,
            num_players,
            cfg: &cfg,
            delay_cfg,
            budget,
            probs,
            delay_script: self.delay_script.script(),
            tenhou: tenhou_state.as_ref(),
        };

        // Packet mode (Majsoul): send a protocol frame over the game's own
        // WebSocket instead of clicking the UI. Click mode (and every other
        // platform) falls through to the click path below.
        if platform_kind == crate::config::Platform::Majsoul
            && matches!(cfg.mode, crate::config::MajsoulAutoplayMode::Packet)
        {
            self.handle_packet_action(
                &resp,
                &cfg,
                planned_budget,
                our_seat,
                legal_actions,
                &snapshot,
            )
            .await;
            return;
        }

        let platform: &dyn PlatformAutoplay = match platform_kind {
            crate::config::Platform::Tenhou => &self.tenhou,
            crate::config::Platform::RiichiCity => &self.riichi,
            _ => &self.majsoul,
        };
        // Every reply that gets here is one the engine asked for: the bot
        // manager only reacts where our seat can act (`bot::manager`), so a
        // `None` means a decline and not "nothing to say". Belt and braces
        // all the same — one window is answered once. A second reply for it
        // is at best a wasted press and at worst one aimed at whatever
        // replaced it.
        if let Some(w) = planned_window {
            if self.state.acted_window == Some(w.opened_at) {
                debug!("autoplay: already acted on this window; ignoring extra bot reply");
                return;
            }
        }

        let plan = platform.plan(&action_ctx);
        if plan.steps.is_empty() {
            return;
        }
        debug!(
            "autoplay: action={:?} steps={}",
            resp.action,
            plan.steps.len(),
        );

        // Only a click needs to know where the canvas is. A plan built of
        // `Send` steps talks to the page's socket and would otherwise be
        // held hostage by a rect query it never uses.
        let needs_canvas = plan.steps.iter().any(|s| matches!(s, Step::Click { .. }));
        let rect = if needs_canvas {
            // Cache + TTL. If we can't resolve one, drop the click — the page
            // handle isn't ready yet (e.g. user still on the lobby), or the
            // chromium backend isn't running at all.
            match self.canvas_rect_resolve().await {
                Some(r) => Some(r),
                None => {
                    warn!(
                        "autoplay: no canvas rect — skipping click for {:?}",
                        resp.action
                    );
                    return;
                }
            }
        } else {
            None
        };

        // Ticket taken before the first press: the check afterwards asks
        // whether the client sent *another* input command, so an input from
        // the previous decision cannot be mistaken for this one landing.
        let ticket = self.ctx.input_watch.ticket();

        // Whether this plan declares riichi: the reach declaration and its
        // tile go out in one plan (the declaring discard is pre-resolved onto
        // `Reach.pai`), so the reach action alone identifies it.
        let declares_reach = matches!(resp.action, MjaiEvent::Reach { .. });

        if let Some(w) = planned_window {
            self.state.acted_window = Some(w.opened_at);
        }

        // Riichi City plans are [Sleep, SendFrame]: run them as their own
        // task instead of inline. Inline execution serialized every
        // decision's sleeps, window waits, and verify pauses, so queued
        // plans stacked — measured injections drifting 2→17s later as a
        // session progressed. The task gates its send on the identity of
        // the decision window it was planned against (see
        // `execute_riichi_frame`), so parallel tasks cannot send into a
        // later window by mistake.
        if platform_kind == crate::config::Platform::RiichiCity {
            let mut sleep_ms = 0;
            let mut frame: Option<Vec<u8>> = None;
            for step in &plan.steps {
                match step {
                    Step::Sleep { duration_ms } => sleep_ms = *duration_ms,
                    Step::SendFrame(bytes) => frame = Some(bytes.clone()),
                    other => warn!("autoplay: unexpected riichi plan step {other:?}"),
                }
            }
            let Some(frame) = frame else {
                return;
            };
            let inject = self.ctx.inject.clone();
            let verify_input_ms = cfg.verify_input_ms;
            let retries = cfg.click_retries;
            let action = resp.action.clone();
            let window_open_at_plan = inject.window_is_open();
            let window_at_plan = inject.window_opened_at();
            let plan_created = Instant::now();
            tauri::async_runtime::spawn(async move {
                execute_riichi_frame(
                    sleep_ms as u64,
                    frame,
                    action,
                    inject,
                    verify_input_ms,
                    retries,
                    window_open_at_plan,
                    window_at_plan,
                    plan_created,
                )
                .await;
            });
            return;
        }

        let mut window_checked = false;
        for step in &plan.steps {
            match step {
                Step::Sleep { duration_ms } => {
                    tokio::time::sleep(Duration::from_millis(*duration_ms as u64)).await;
                }
                Step::AwaitReady { timeout_ms } => {
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot wait for the client");
                        return;
                    };
                    if !Self::await_turn_ready(page, *timeout_ms).await {
                        return;
                    }
                }
                Step::DomClick { selectors, label } => {
                    if self.tenhou_window_moved(planned_window) {
                        warn!(
                            "autoplay: decision window closed mid-delay — dropping stale {label}"
                        );
                        return;
                    }
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot press {label}");
                        return;
                    };
                    // The client only renders the buttons it is currently
                    // offering, so a selector that matches nothing means the
                    // decision resolved while we were thinking. Report it and
                    // stop rather than pressing something else.
                    // The buttons exist only between the end of the
                    // client's animation for the triggering frame and
                    // whatever resolves the window, and neither edge is
                    // observable from here — a single look loses the race
                    // either way. Wait for the element instead, bounded by
                    // what is left of the turn.
                    match self.press_when_offered(page, selectors, label).await {
                        true => {}
                        false => return,
                    }
                }
                Step::Discard { tile_index } => {
                    // The client's discard handler applies locally whether or
                    // not it is our turn — its own UI only reaches it while
                    // one is — so a stale call desyncs the board, not just
                    // wastes a frame. The riichi plan is the exception: its
                    // own button press replaces the window (the server acks
                    // the declaration and the bridge re-opens it), and the
                    // tile it owes is still this plan's to throw.
                    if discard_needs_window_guard(&resp.action)
                        && self.tenhou_window_moved(planned_window)
                    {
                        warn!(
                            "autoplay: decision window closed mid-delay — dropping stale discard"
                        );
                        return;
                    }
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot discard");
                        return;
                    };
                    match crate::autoplay::cdp_input::discard_tile(page, *tile_index).await {
                        Ok(true) => info!("autoplay: discarded tile index {tile_index}"),
                        Ok(false) => {
                            warn!(
                                "autoplay: the client script was not instrumented, so its \
                                 discard handler is unreachable; skipping"
                            );
                            return;
                        }
                        Err(e) => {
                            warn!("autoplay: discard failed: {e:#}");
                            return;
                        }
                    }
                }
                Step::Click { x_norm, y_norm } => {
                    let Some(rect) = rect else {
                        warn!("autoplay: click step with no canvas rect; skipping");
                        continue;
                    };
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
                                    resp.action
                                );
                                return;
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
                        warn!("autoplay: no page handle — aborting click sequence");
                        return;
                    };
                    // A lost press usually costs nothing — the decision
                    // window closes and the client passes. A lost *riichi*
                    // press is different: the discard that follows it still
                    // goes out, so the hand throws its riichi tile with no
                    // riichi behind it. That press gets the sturdier shape
                    // from the first attempt rather than only on a retry.
                    if let Err(e) = dispatch_click_shaped(
                        page,
                        px,
                        py,
                        cfg.hover_delay_ms,
                        cfg.click_hold_ms,
                        declares_reach,
                    )
                    .await
                    {
                        warn!("autoplay: dispatch_click failed: {e:#}");
                        return;
                    }
                    drop(page_guard);
                }
                // Riichi City SendFrames never reach this loop — they are
                // spawned above as per-decision tasks.
                Step::SendFrame(_) => unreachable!("riichi frames run as tasks"),
            }
        }

        // Did the client accept any of that? A swallowed click reports
        // success like any other — the page dispatched the events and the
        // engine ignored them — so the proof has to come from the client's
        // own uplink.
        // Only canvas clicks need proving. The Tenhou steps run the client's
        // own handlers — a DOM press or its discard call either resolved or
        // reported that it did not, with no swallowed-input case in between —
        // and `rect` is `None` for a plan built of those, which skips this.
        if let (true, Some(rect)) = (cfg.verify_input_ms > 0, rect) {
            // What counts as proof. A discard plan is proven by any input.
            // Every other plan pressed action buttons, and for those a
            // plain discard can only be the client's own turn-timeout
            // tsumogiri — it arrives with a human-scale `timeuse` and no
            // `auto_operation` flag, so the wire-level filter cannot drop
            // it, and accepting it would report retries as registered at
            // the exact moments the presses had failed.
            let require_non_discard = !matches!(resp.action, MjaiEvent::Dahai { .. });
            let registered = self
                .verify_and_retry(
                    rect,
                    &plan,
                    ticket,
                    &cfg,
                    planned_budget,
                    &resp.action,
                    require_non_discard,
                )
                .await;
            // Once the client stops accepting presses it tends to stay that
            // way for the rest of the game — the failures do not come back
            // one at a time, they arrive and then every remaining decision
            // runs to timeout. A page reload reconnects into the hand
            // through the bridge's GameRestore path, so the way out costs a
            // reconnect rather than the game.
            if registered {
                self.state.dead_clicks = 0;
            } else {
                self.state.dead_clicks += 1;
                if cfg.reload_after_failures > 0
                    && self.state.dead_clicks >= cfg.reload_after_failures
                {
                    warn!(
                        decisions = self.state.dead_clicks,
                        "autoplay: the client has stopped accepting presses — reloading to recover"
                    );
                    let _ = self.notify.send(
                        crate::schema::Notification::warn("Reloading the game")
                            .body("Clicks stopped registering; reconnecting to recover."),
                    );
                    self.state.dead_clicks = 0;
                    self.reload_page().await;
                }
            }
            // A riichi plan that produced an input command has not
            // necessarily declared anything: if the button press was lost
            // and only the tile press landed, the client sends a plain
            // discard, which the presence check happily accepts. The wire
            // tells them apart, so check, and say so — the tile is gone
            // either way, but the player is now in a hand they did not
            // choose and nothing else would tell them. It is also the one
            // window where the bot's own view can drift: if the declaring
            // tile came from an autoplay reach follow-up (#257), the bot was
            // fed a reach that then did not happen, so flag that too.
            if declares_reach
                && self.ctx.input_watch.sent_since(ticket)
                && !self.ctx.input_watch.reach_since(ticket)
            {
                warn!(
                    "autoplay: riichi was planned for {:?} but the client sent a plain discard — the declaration press was lost and the tile went out without it",
                    resp.action
                );
                let _ = self.notify.send(
                    crate::schema::Notification::error("Riichi did not go through").body(
                        "The declaration press was lost and the tile was discarded without it. This hand is not in riichi — and the bot may still believe it declared, so its reads for the rest of this hand can be off.",
                    ),
                );
            }
        }
    }

    /// Wait until the client is taking input, or give up.
    ///
    /// The turn does not begin when the frame arrives. Tenhou's server sends
    /// as fast as the seats answer — against instant opponents that means
    /// three seats' actions in one burst — and the client then animates them
    /// for seconds before drawing its buttons and starting its clock. Timing
    /// anything from frame arrival times it from the wrong instant, which is
    /// why fixed delays kept landing either side of the window.
    ///
    /// The client raises its clock display and its highlight together, so the
    /// highlight appearing is the readiness signal.

    /// Packet-mode Majsoul path: build a wire frame for the bot's action
    /// and dispatch it through the game's WebSocket bridge (see
    /// `try_packet_action`). Falls back to nothing — packet mode never
    /// clicks.
    async fn handle_packet_action(
        &mut self,
        resp: &BotResponse,
        cfg: &crate::config::MajsoulAutoplayConfig,
        planned_budget: Option<crate::autoplay::budget::TimeBudget>,
        our_seat: u8,
        legal_actions: Vec<Action>,
        snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    ) {
        let action_retry_guard = retry_guard_for_action(&resp.action, our_seat, &snapshot);

        let use_packet = matches!(cfg.mode, MajsoulAutoplayMode::Packet);
        let allow_click = matches!(cfg.mode, MajsoulAutoplayMode::Click);

        let packet_action = normalize_packet_action(
            &resp.action,
            our_seat,
            &snapshot,
            self.state.last_self_tsumo.as_deref(),
        );
        if use_packet && packet_action_allowed(&packet_action, our_seat, &snapshot, &legal_actions)
        {
            let dealer_first_discard = matches!(packet_action, MjaiEvent::Dahai { .. })
                && is_dealer_first_discard(&snapshot, our_seat);
            let packet_delay_ms = sample_packet_delay(&cfg, dealer_first_discard);
            tokio::time::sleep(Duration::from_millis(packet_delay_ms.into())).await;
            if planned_budget.is_some() {
                let current = self.ctx.time_budget.read().ok().and_then(|g| *g);
                if current.map(|b| b.opened_at) != planned_budget.map(|b| b.opened_at) {
                    debug!("autoplay: packet decision window closed during delay");
                    return;
                }
            }
            if let Some(guard) = action_retry_guard.as_ref() {
                if !self.action_still_pending(guard).await {
                    debug!(
                        "autoplay: packet action {:?} became stale during delay",
                        packet_action
                    );
                    return;
                }
            }
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
                                return;
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
                    return;
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
        if let Some(result) = self.ctx.send_mitm_packet(&frame).await {
            return match result {
                Ok(()) => {
                    info!("autoplay: queued MITM packet for {:?}", action);
                    PacketDispatch::Sent
                }
                Err(_) => {
                    debug!("autoplay: MITM packet WebSocket is closed for {:?}", action);
                    PacketDispatch::HookNoOpenSocket
                }
            };
        }
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
        // Path-B: a riichi-declaring plan resolved its declaration and its
        // discard into one send. The bot never saw the reach, so inject a
        // synthetic one so it emits the riichi-declaring dahai the client
        // still owes.
        if inject_reach_for_followup {
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
        let _ = action;
    }
    async fn await_turn_ready(page: &Page, timeout_ms: u32) -> bool {
        const POLL_INTERVAL: Duration = Duration::from_millis(120);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let started = Instant::now();
        loop {
            match crate::autoplay::cdp_input::turn_clock_running(page).await {
                Ok(true) => {
                    debug!(
                        "autoplay: client ready after {}ms",
                        started.elapsed().as_millis()
                    );
                    return true;
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("autoplay: readiness probe failed: {e:#}");
                    return false;
                }
            }
            if Instant::now() >= deadline {
                warn!(
                    "autoplay: client never started its clock within {timeout_ms}ms; \
                     skipping this decision"
                );
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Press `selector` as soon as the client offers it.
    ///
    /// A button is only in the DOM between the end of the client's animation
    /// for the frame that opened the window and whatever closes it. Neither
    /// edge is visible from here and the gap moves with animation length, so
    /// a single `querySelector` races it — the first live run pressed
    /// successfully three times and missed four, all with the client offering
    /// nothing at the instant we looked. Polling turns that race into a
    /// bounded wait.
    async fn press_when_offered(&self, page: &Page, selectors: &[String], label: &str) -> bool {
        const POLL_INTERVAL: Duration = Duration::from_millis(120);
        const WAIT_BUDGET: Duration = Duration::from_millis(2_400);

        let deadline = Instant::now() + WAIT_BUDGET;
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            match crate::autoplay::cdp_input::click_dom(page, selectors).await {
                Ok(true) => {
                    info!("autoplay: pressed {label} ({selectors:?}) after {attempts} look(s)");
                    return true;
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("autoplay: pressing {label} failed: {e:#}");
                    return false;
                }
            }
            if Instant::now() >= deadline {
                let offered = crate::autoplay::cdp_input::list_action_buttons(page).await;
                warn!(
                    "autoplay: {label} button ({selectors:?}) never appeared in {:?}; \
                     client is offering slots {offered:?} (its own order: highest first)",
                    WAIT_BUDGET
                );
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Has the Tenhou decision window been replaced since we planned?
    ///
    /// The delay model sleeps for seconds, and a window can resolve in that
    /// time — a claim we declined, or simply the next player acting. Acting on
    /// the stale plan then answers a decision that is already over. The
    /// window's `opened_at` is its identity, so a slot no longer holding the
    /// same instant means what we planned for is gone.
    ///
    /// `None` planned means we never had a window to go stale (other
    /// platforms), so nothing is dropped.
    fn tenhou_window_moved(
        &self,
        planned: Option<crate::autoplay::tenhou_state::DecisionWindow>,
    ) -> bool {
        let Some(planned) = planned else {
            return false;
        };
        let current = self
            .ctx
            .tenhou_state
            .read()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.window));
        current.map(|w| w.opened_at) != Some(planned.opened_at)
    }

    /// Wait for the client's input command; press again if it never came.
    ///
    /// Retrying is bounded and gated on the decision window still being the
    /// one the plan was made for: if our action did land and the answer was
    /// merely slow, the server's echo closes that window and the retry is
    /// dropped rather than pressed into the next decision.
    #[allow(clippy::too_many_arguments)]
    async fn verify_and_retry(
        &self,
        rect: CanvasRect,
        plan: &crate::autoplay::PlanResult,
        ticket: InputTicket,
        cfg: &crate::config::MajsoulAutoplayConfig,
        planned_budget: Option<crate::autoplay::budget::TimeBudget>,
        action: &MjaiEvent,
        require_non_discard: bool,
    ) -> bool {
        let clicks: Vec<(f64, f64)> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Click { x_norm, y_norm } => Some((*x_norm, *y_norm)),
                Step::Sleep { .. }
                | Step::DomClick { .. }
                | Step::Discard { .. }
                | Step::AwaitReady { .. }
                | Step::SendFrame(_) => None,
            })
            .collect();
        if clicks.is_empty() {
            return true;
        }

        for attempt in 0..=cfg.click_retries {
            if self
                .wait_for_input(ticket, cfg.verify_input_ms, require_non_discard)
                .await
            {
                if attempt > 0 {
                    info!("autoplay: retry {attempt} registered for {action:?}");
                }
                return true;
            }
            if attempt == cfg.click_retries {
                break;
            }
            if !self.window_still_open(planned_budget) {
                debug!(
                    "autoplay: no input seen for {action:?}, but the decision window has moved on — not retrying"
                );
                return true;
            }
            let again = retry_slice(&clicks, attempt);
            // The coordinates are in the message on purpose: when a button
            // visibly reacts and the action still does not happen, this
            // line is what says which slot was aimed at.
            warn!(
                "autoplay: no input command after clicking {action:?} at {clicks:?} (hover {}ms, hold {}ms); pressing {} of {} click(s) again (attempt {})",
                cfg.hover_delay_ms,
                cfg.click_hold_ms,
                again.len(),
                clicks.len(),
                attempt + 1
            );
            // Retries vary the press rather than repeating it verbatim.
            // A press that lands on the right control and still does not
            // commit is not a positioning problem, so pressing the same way
            // again has nothing new to offer: the second attempt holds
            // longer, the third also nudges the cursor mid-press.
            let hold = retry_hold_ms(cfg.click_hold_ms, attempt);
            let jiggle = attempt >= 1;
            for (i, (x_norm, y_norm)) in again.iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(Duration::from_millis(u64::from(cfg.inter_click_delay_ms)))
                        .await;
                }
                let (px, py) = rect.pixel(*x_norm, *y_norm);
                if !rect.contains(px, py) {
                    warn!(
                        "autoplay: retry click ({px},{py}) outside canvas rect {:?}; skipping",
                        rect
                    );
                    continue;
                }
                let page_guard = self.ctx.page.read().await;
                let Some(page) = page_guard.as_ref() else {
                    warn!("autoplay: no page handle — abandoning retry for {action:?}");
                    return false;
                };
                if let Err(e) =
                    dispatch_click_shaped(page, px, py, cfg.hover_delay_ms, hold, jiggle).await
                {
                    warn!("autoplay: retry dispatch_click failed: {e:#}");
                }
            }
        }
        // The timings are in the message because a config written before a
        // default changed keeps its stored values, and "the new default
        // never reached me" is otherwise indistinguishable from "the click
        // is landing in the wrong place".
        warn!(
            "autoplay: {action:?} never produced an input command after {} attempt(s) at {clicks:?} (hover {}ms, hold {}ms); if the button visibly reacts to these coordinates the press is being ignored — try longer timings under Settings -> Autoplay",
            cfg.click_retries + 1,
            cfg.hover_delay_ms,
            cfg.click_hold_ms
        );
        false
    }

    /// Poll for an input command until `timeout_ms` runs out. Polling (not
    /// one flat sleep) so a click that worked returns almost immediately and
    /// only a lost one pays the full wait. With `require_non_discard`, a
    /// plain discard does not satisfy the wait — the proof standard for a
    /// plan whose clicks were all action buttons.
    async fn wait_for_input(
        &self,
        ticket: InputTicket,
        timeout_ms: u32,
        require_non_discard: bool,
    ) -> bool {
        const POLL: Duration = Duration::from_millis(20);
        let deadline = std::time::Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            let seen = if require_non_discard {
                self.ctx.input_watch.non_discard_since(ticket)
            } else {
                self.ctx.input_watch.sent_since(ticket)
            };
            if seen {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Whether the decision window the plan was made against is still the
    /// live one. With no budget tracked there is no signal, and the answer
    /// is "yes" — behaviour off-Majsoul is unchanged.
    fn window_still_open(&self, planned: Option<crate::autoplay::budget::TimeBudget>) -> bool {
        let Some(planned) = planned else {
            return true;
        };
        let current = self.ctx.time_budget.read().ok().and_then(|g| *g);
        current.map(|b| b.opened_at) == Some(planned.opened_at)
    }

    async fn action_still_pending(&self, guard: &ActionRetryGuard) -> bool {
        let tracker = self.tracker.lock().await;
        let Some(snapshot) = tracker.snapshot() else {
            return false;
        };
        snapshot_fingerprint(&snapshot, guard.our_seat).as_ref() == Some(&guard.before)
    }

    async fn reload_page(&mut self) {
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref().cloned() else {
            warn!("autoplay: cannot reload — no page handle");
            return;
        };
        drop(page_guard);
        if let Err(e) = page.reload().await {
            warn!("autoplay: page reload failed: {e:#}");
        }
        // The canvas is rebuilt on reload; drop the cached rect so the
        // next click re-measures it.
        self.state.canvas_rect_at = None;
        *self.ctx.canvas_rect.write().await = None;
    }

    fn handle_mjai_event(&mut self, ev: &MjaiEvent) {
        match ev {
            MjaiEvent::StartGame { id, .. } => {
                if let Some(task) = self.rematch_task.take() {
                    task.abort();
                }
                // Capture our seat directly from the StartGame event rather
                // than going through the tracker. This avoids the try_lock
                // race entirely and makes cached_our_seat available from
                // the very first event of the game.
                let seat = *id;
                self.state = ManagerState::default();
                self.state.cached_our_seat = seat;
                self.ctx
                    .auto_join_set_phase(crate::autoplay::context::AutoJoinPhase::InGame);
            }
            MjaiEvent::EndGame { .. } => {
                self.state = ManagerState::default();
                if let Some(task) = self.rematch_task.take() {
                    task.abort();
                }
                info!("auto-join: EndGame received; starting settlement flow");
                let cfg = Arc::clone(&self.cfg);
                let ctx = Arc::clone(&self.ctx);
                self.rematch_task = Some(tokio::spawn(async move {
                    let settings = {
                        let cfg = cfg.read().await;
                        if !cfg.autoplay.enabled || !cfg.autoplay.majsoul.auto_join_game {
                            return;
                        }
                        cfg.autoplay.majsoul.clone()
                    };
                    ctx.auto_join_record_completed();
                    crate::autoplay::majsoul::rematch::run(ctx, settings).await;
                }));
            }
            MjaiEvent::StartKyoku { .. } | MjaiEvent::EndKyoku => {
                // Per-kyoku reset: keep last seen rect cache and cached seat,
                // drop everything else. Keep last_kawa_tile as None so
                // push_random_pre_delay uses the max delay (opening-hand guard).
                let canvas_at = self.state.canvas_rect_at;
                let cached_seat = self.state.cached_our_seat;
                // The dead-click count survives too: a client that has
                // stopped accepting presses stays that way across the kyoku
                // boundary, and zeroing it here would need the failures to
                // land inside one hand before anything recovered them.
                let dead_clicks = self.state.dead_clicks;
                self.state = ManagerState::default();
                self.state.canvas_rect_at = canvas_at;
                self.state.cached_our_seat = cached_seat;
                self.state.dead_clicks = dead_clicks;
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

    fn start_lobby_watcher(&mut self) {
        let cfg = Arc::clone(&self.cfg);
        let ctx = Arc::clone(&self.ctx);
        self.rematch_task = Some(tokio::spawn(async move {
            crate::autoplay::majsoul::rematch::wait_for_main_menu(ctx, cfg).await;
        }));
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

/// Does a `Step::Discard` for this action require the decision window it
/// was planned against to still be open?
///
/// Everything but a riichi does: the window's identity is how a discard that
/// out-waited its turn — the client timed out and threw for us, or a human
/// beat the bot to it — is told apart from one that is still owed. A riichi
/// plan cannot use that test, because passing its own declaration button is
/// what replaces the window (the server acks with `REACH step=1` and the
/// bridge re-opens it for the tile), so the move is expected rather than
/// evidence of staleness — and the tile must still go out or the client sits
/// on its clock and times the hand out.
fn discard_needs_window_guard(action: &MjaiEvent) -> bool {
    !matches!(action, MjaiEvent::Reach { .. })
}

/// Which clicks to press again on retry `attempt` (0-based).
///
/// In a multi-click plan — a chi/pon/kan whose candidate row needs
/// disambiguating, or a Path-A riichi — the *last* click is both the one
/// that commits the action and the one most likely to have been swallowed:
/// it fires `inter_click_delay_ms` after the previous press, while the
/// candidate row is still animating in, whereas the opening click follows
/// the full thinking delay against a settled UI.
///
/// So the first retry presses only that last click. If the row never opened
/// because the *opening* click was the one that missed, it lands on empty
/// table and does nothing — a harmless way to be wrong, unlike re-pressing
/// a chi button whose row is already open, whose effect we cannot predict.
/// A second retry then replays the whole sequence on the theory that the
/// opening click was the one lost.
fn retry_hold_ms(base: u32, attempt: u32) -> u32 {
    base.saturating_mul(attempt + 2).min(2_000)
}

fn future_window_grace(action: &MjaiEvent) -> Duration {
    match action {
        MjaiEvent::Dahai { .. }
        | MjaiEvent::Reach { .. }
        | MjaiEvent::Ryukyoku { deltas: None } => Duration::from_secs(15),
        _ => Duration::from_secs(3),
    }
}

fn retry_slice(clicks: &[(f64, f64)], attempt: u32) -> &[(f64, f64)] {
    if attempt == 0 && clicks.len() > 1 {
        &clicks[clicks.len() - 1..]
    } else {
        clicks
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

fn sample_packet_delay(cfg: &crate::config::MajsoulAutoplayConfig, dealer_first: bool) -> u32 {
    let lo = cfg.packet_delay_min_ms.min(cfg.packet_delay_max_ms);
    let hi = cfg.packet_delay_min_ms.max(cfg.packet_delay_max_ms);
    let base = rand::rng().random_range(lo..=hi);
    base.saturating_add(if dealer_first {
        cfg.packet_dealer_first_discard_extra_delay_ms
    } else {
        0
    })
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
    let moqie = !is_dealer_first_discard(snapshot, our_seat) && drawn == Some(pai.as_str());
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
            let moqie = !is_dealer_first_discard(snapshot, our_seat) && drawn == Some(pai.as_str());
            hints.self_operation_index = Some(0);
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

/// Execute one Riichi City decision off the plan loop: sleep the (already
/// clamped) think time, wait for OUR decision window, hold the minimum
/// visible think, send, and verify the server's ack — retrying the send if
/// no ack lands.
///
/// Parallel tasks are safe because each gates its send on the *identity*
/// of the window it was planned against (`window_opened_at`): the window
/// that was open when the plan was created, or — for own-turn actions —
/// the first window to open after planning. A task whose window resolved
/// while it slept aborts instead of sending into whatever window is open
/// now.
#[allow(clippy::too_many_arguments)]
async fn execute_riichi_frame(
    sleep_ms: u64,
    frame: Vec<u8>,
    action: MjaiEvent,
    inject: crate::autoplay::inject::SharedInjectBus,
    verify_input_ms: u32,
    retries: u32,
    window_open_at_plan: bool,
    window_at_plan: Option<Instant>,
    plan_created: Instant,
) {
    if !inject.in_game() {
        debug!("autoplay: dropping frame for {action:?} — no game in progress");
        return;
    }

    // Resolve our window identity.
    let identity = if window_open_at_plan {
        // The window open at planning time is ours; require it to still
        // be the one open.
        let Some(id) = window_at_plan else {
            return;
        };
        id
    } else {
        // Planned before any window opened: either an own-turn action
        // whose window trails the deal animation, or a claim whose offer
        // frame lost the race against the bot response (measured:
        // responses can be planned milliseconds before the bridge
        // processes the offer). Wait for the first window to open after
        // planning, bounded per action kind — acting before the window
        // opens is rejected (rsp code 1).
        let deadline = plan_created + future_window_grace(&action);
        loop {
            if inject.window_is_open() {
                if let Some(opened) = inject.window_opened_at() {
                    if opened >= plan_created {
                        break opened;
                    }
                }
            }
            if Instant::now() >= deadline {
                debug!("autoplay: {action:?} window never opened; dropping");
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    };

    // Wait until our window is open.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !(inject.window_is_open() && inject.window_opened_at() == Some(identity)) {
        if Instant::now() >= deadline
            || (inject.window_is_open() && inject.window_opened_at() != Some(identity))
        {
            debug!("autoplay: dropping stale {action:?} — the window moved on");
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Hold the humanizer's think time, measured from the WINDOW OPENING —
    // the Lua contract: delay_ms is what the server observes between
    // offering the decision and receiving the action. Anchoring to the
    // window (not the triggering event) is what keeps the dealer's
    // opening discard human-paced: the deal animation precedes the window
    // and must not eat the think. Capped well under the ~15s window;
    // floored so the timer is always visibly up before the send.
    const MIN_VISIBLE_THINK_MS: u64 = 2_000;
    const THINK_CAP_MS: u64 = 10_000;
    let target_ms = sleep_ms.clamp(MIN_VISIBLE_THINK_MS, THINK_CAP_MS);
    let elapsed_ms = identity.elapsed().as_millis() as u64;
    if elapsed_ms < target_ms {
        tokio::time::sleep(Duration::from_millis(target_ms - elapsed_ms)).await;
    }
    // The window may have resolved during the think.
    if !(inject.window_is_open() && inject.window_opened_at() == Some(identity)) {
        debug!("autoplay: dropping stale {action:?} — the window moved on");
        return;
    }

    let ticket = inject.rsp_ticket();
    let mut acked = false;
    for attempt in 0..=retries {
        if !inject.send(InjectFrame {
            gameplay: true,
            bytes: frame.clone(),
        }) {
            warn!(
                "autoplay: no injection relay subscribed — is capture running? \
                   dropping frame for {action:?}"
            );
            return;
        }
        info!("autoplay: injected frame for {action:?} (attempt {attempt})");
        // The server's round trip is ~200ms; the floor keeps a short
        // verify_input_ms from spuriously retrying.
        let wait = u64::from(verify_input_ms.max(500));
        let deadline = Instant::now() + Duration::from_millis(wait);
        loop {
            if inject.rsp_since(ticket) {
                acked = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if acked {
            break;
        }
        warn!("autoplay: no rsp_game_action within {wait}ms for {action:?} (attempt {attempt})");
    }
    if acked {
        let code = inject.last_rsp_code();
        if code != 0 {
            warn!("autoplay: the server rejected {action:?} (rsp code {code})");
        } else if retries > 0 {
            debug!("autoplay: rsp ok for {action:?}");
        }
    } else {
        warn!("autoplay: {action:?} was never answered by the server — the action did not happen");
    }
}

/// Spawn point for the autoplay loop. Wired by `crate::lib::run` so the
/// `tauri::async_runtime` Tokio runtime is the host.
pub async fn run_autoplay_manager(
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    mjai_bus: MjaiBus,
    response_bus: BotResponseBus,
    notify: NotifyBus,
    config_dir: std::path::PathBuf,
) -> anyhow::Result<()> {
    AutoplayManager::new(cfg, ctx, tracker, mjai_bus, notify, config_dir)
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
            crate::event_bus::notify_bus(),
            std::env::temp_dir(),
        )
    }

    #[test]
    fn packet_delay_uses_its_own_range_and_dealer_extra() {
        let cfg = crate::config::MajsoulAutoplayConfig {
            packet_delay_min_ms: 123,
            packet_delay_max_ms: 123,
            packet_dealer_first_discard_extra_delay_ms: 456,
            ..Default::default()
        };
        assert_eq!(sample_packet_delay(&cfg, false), 123);
        assert_eq!(sample_packet_delay(&cfg, true), 579);
    }

    /// Regression: `cached_our_seat` must be populated immediately when
    /// `StartGame` is received, before any bot response fires. Previously
    /// the seat was only cached inside `handle_bot_response`, so the first
    /// `Tsumo` event on the opening draw could arrive before the bot had
    /// responded and `last_self_tsumo` would be silently missed.
    /// A client that has stopped accepting presses stays that way across
    /// a kyoku boundary — the failures run to the end of the game — so the
    /// count has to survive one, or the threshold would only ever be
    /// reached by failures packed into a single hand. It does reset for a
    /// new game, which is a new page state.
    #[test]
    fn dead_click_count_survives_a_kyoku_but_not_a_game() {
        let mut m = make_manager();
        m.state.dead_clicks = 2;
        m.handle_mjai_event(&MjaiEvent::EndKyoku);
        assert_eq!(m.state.dead_clicks, 2, "the client is still not listening");

        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(0),
            num_players: 4,
            game_meta: None,
        });
        assert_eq!(m.state.dead_clicks, 0, "a new table is a fresh start");
    }

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
            game_meta: None,
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
            game_meta: None,
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
            game_meta: None,
        });
        assert_eq!(m.state.cached_our_seat, Some(0));

        // New game, observer mode - no seat.
        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: None,
            num_players: 4,
            game_meta: None,
        });
        assert!(
            m.state.cached_our_seat.is_none(),
            "stale seat must be cleared"
        );
    }
}
