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

use crate::autoplay::cdp_input::{dispatch_click_shaped, evaluate_canvas_rect};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::inject::InjectFrame;
use crate::autoplay::majsoul::MajsoulAutoplay;
use crate::autoplay::platform::{ActionContext, PlatformAutoplay, Step};
use crate::autoplay::riichi_city::RiichiCityAutoplay;
use crate::autoplay::tenhou::TenhouAutoplay;
use crate::autoplay::verify::InputTicket;
use crate::bot::BotResponse;
use crate::config::AppConfig;
use crate::event_bus::{BotResponseBus, MjaiBus, NotifyBus};
use crate::game_state::tracker::GameTracker;
use crate::schema::MjaiEvent;
use chromiumoxide::page::Page;
use riichienv_core::action::Action;
use riichienv_core::state::legal_actions::GameStateLegalActions;
use riichienv_core::state_3p::legal_actions::GameState3PLegalActions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast::error::RecvError, Mutex, RwLock};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

/// How long before a cached `CanvasRect` is treated as stale and re-queried.
const CANVAS_RECT_TTL: Duration = Duration::from_secs(30);

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
}

#[derive(Default)]
struct ManagerState {
    /// When the most recent mjai event arrived. The decision-window wait
    /// anchors to this, NOT to when the bot response is handled: responses
    /// queue behind earlier plans, and a handle-time anchor can land AFTER
    /// the window already opened — the wait would then block on a condition
    /// that can never become true and burn its full 15s bound (the round-
    /// opening freezes of 2026-08-20).
    last_event_at: Option<Instant>,
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
            round_advance_watcher(advance_cfg, advance_inject, advance_bus).await;
        });

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
                        self.handle_session_event(&ev).await;
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

    /// Session advance: `StartGame` marks a queued match as landed;
    /// `EndGame` counts it and queues the next after the inter-game
    /// delay. (`EndKyoku` is the round-advance watcher's, not ours.)
    async fn handle_session_event(&mut self, ev: &MjaiEvent) {
        let session = self.ctx.session.clone();
        match ev {
            MjaiEvent::StartGame { .. } => session.note_game_started(),
            MjaiEvent::EndGame { .. } => {
                if !session.is_active() {
                    return;
                }
                let status = session.status();
                if !session.on_game_finished() {
                    let _ = self.notify.send(
                        crate::schema::Notification::info("Autoplay session finished").body(
                            format!(
                                "Played {} game(s){}; autoplay session ended.",
                                status.games_completed + 1,
                                status
                                    .target_games
                                    .map(|t| format!(" of {t}"))
                                    .unwrap_or_default(),
                            ),
                        ),
                    );
                    return;
                }
                self.schedule_next_queue().await;
            }
            _ => {}
        }
    }

    /// Queue the next Riichi City match after the inter-game delay.
    async fn schedule_next_queue(&self) {
        let rc = self.cfg.read().await.autoplay.riichi_city.clone();
        let delay = crate::autoplay::session::inter_game_delay(rc.inter_game_delay_ms);
        let cfg = self.cfg.clone();
        let inject = self.ctx.inject.clone();
        let session = self.ctx.session.clone();
        let notify = self.notify.clone();
        let config_dir = self.config_dir.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            run_queue_task(cfg, inject, session, notify, config_dir).await;
        });
    }

    async fn handle_bot_response(&mut self, resp: BotResponse) {
        // The decision-window wait anchors to the triggering mjai event
        // (see `ManagerState::last_event_at` and the SendFrame arm below).
        // Falls back to now only if no event was ever seen.
        let trigger_at = self.state.last_event_at.unwrap_or_else(Instant::now);
        // Re-read config every iteration so `cfg.autoplay.enabled` can be
        // toggled at runtime via the Settings UI without restarting.
        let cfg_guard = self.cfg.read().await;
        if !cfg_guard.autoplay.enabled {
            return;
        }
        let cfg = cfg_guard.autoplay.majsoul.clone();
        let delay_cfg = cfg_guard.autoplay.delay.clone();
        let delay_cap_ms = delay_cfg.no_budget_cap_ms;
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

        let mut plan = platform.plan(&action_ctx);
        if plan.steps.is_empty() {
            return;
        }
        // Riichi City: the delay script's fat tail has no server budget to
        // clamp it (there is none on this platform), and a long-thought
        // sample on a decision that queued behind earlier plans lands tens
        // of seconds into a ~25s window — occasionally too late. Bound the
        // total by the configured no-budget cap and credit the time already
        // spent since the triggering event, so a late-handled response
        // shortens its own sleep instead of stacking on top of the queue.
        if platform_kind == crate::config::Platform::RiichiCity {
            // Cap below the configured no-budget cap: observed Riichi City
            // decision windows run ~15s (5 fixed + ~10), so a full 15s
            // think after any queueing overshoots the window.
            let elapsed_ms = trigger_at.elapsed().as_millis() as u32;
            clamp_first_sleep(&mut plan, delay_cap_ms.min(10_000), elapsed_ms);
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
                Step::SendFrame(bytes) => {
                    // A gameplay frame with no game in progress is void —
                    // typically a declined-call plan still draining from
                    // the finished game's backlog.
                    if !self.ctx.inject.in_game() {
                        debug!(
                            "autoplay: dropping frame for {:?} — no game in progress",
                            resp.action
                        );
                        return;
                    }
                    // Wait for the decision window to open: acting before
                    // it does is rejected (rsp code 1). State, not
                    // timestamps — the dealer's opening window trails the
                    // deal by seconds, and clock anchors drift when plans
                    // queue.
                    if !self.ctx.inject.window_is_open() {
                        let deadline = Instant::now() + Duration::from_secs(15);
                        while Instant::now() < deadline {
                            if self.ctx.inject.window_is_open() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(150)).await;
                        }
                    }
                    // Keep a minimum visible think after the timer
                    // renders, however the sleep was clamped.
                    const MIN_VISIBLE_THINK_MS: u64 = 2_000;
                    if let Some(opened) = self.ctx.inject.window_opened_at() {
                        let visible_ms = opened.elapsed().as_millis() as u64;
                        if visible_ms < MIN_VISIBLE_THINK_MS {
                            tokio::time::sleep(Duration::from_millis(
                                MIN_VISIBLE_THINK_MS - visible_ms,
                            ))
                            .await;
                        }
                    }
                    let ticket = self.ctx.inject.rsp_ticket();
                    let mut acked = false;
                    for attempt in 0..=cfg.click_retries {
                        if !self.ctx.inject.send(InjectFrame {
                            gameplay: true,
                            bytes: bytes.clone(),
                        }) {
                            warn!(
                                "autoplay: no injection relay subscribed — is capture running? \
                                 dropping frame for {:?}",
                                resp.action
                            );
                            return;
                        }
                        info!(
                            "autoplay: injected frame for {:?} (attempt {attempt})",
                            resp.action
                        );
                        // The server's round trip was ~100ms in the
                        // capture; the floor keeps a short
                        // verify_input_ms from spuriously retrying.
                        let wait = u64::from(cfg.verify_input_ms.max(500));
                        let deadline = Instant::now() + Duration::from_millis(wait);
                        loop {
                            if self.ctx.inject.rsp_since(ticket) {
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
                        warn!(
                            "autoplay: no rsp_game_action within {wait}ms for {:?} \
                             (attempt {attempt})",
                            resp.action
                        );
                    }
                    if acked {
                        let code = self.ctx.inject.last_rsp_code();
                        if code != 0 {
                            warn!(
                                "autoplay: the server rejected {:?} (rsp code {code})",
                                resp.action
                            );
                        } else if cfg.click_retries > 0 {
                            debug!("autoplay: rsp ok for {:?}", resp.action);
                        }
                    } else {
                        warn!(
                            "autoplay: {:?} was never answered by the server — the action \
                             did not happen",
                            resp.action
                        );
                    }
                }
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

    /// Reload the game tab. The bridge reconnects into the in-progress
    /// hand through its `GameRestore` path (`syncGame` replay), so the
    /// cost is a reconnect rather than the game.
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
        // Every event refreshes the decision-window anchor (see
        // `ManagerState::last_event_at`).
        let now = Instant::now();
        self.state.last_event_at = Some(now);
        match ev {
            MjaiEvent::StartGame { id, .. } => {
                // Capture our seat directly from the StartGame event rather
                // than going through the tracker. This avoids the try_lock
                // race entirely and makes cached_our_seat available from
                // the very first event of the game.
                let seat = *id;
                self.state = ManagerState::default();
                self.state.cached_our_seat = seat;
                self.state.last_event_at = Some(now);
            }
            MjaiEvent::EndGame { .. } => {
                self.state = ManagerState::default();
                self.state.last_event_at = Some(now);
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
                self.state.last_event_at = Some(now);
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
            | MjaiEvent::Kakan { actor, .. } => {
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
fn retry_slice(clicks: &[(f64, f64)], attempt: u32) -> &[(f64, f64)] {
    if attempt == 0 && clicks.len() > 1 {
        &clicks[clicks.len() - 1..]
    } else {
        clicks
    }
}

/// Hold duration for retry press number `attempt` (0-based). The original
/// press already ran at `base`, so the first retry — the second press
/// overall — is the one that must hold twice as long; `attempt + 1` would
/// have repeated the original hold verbatim and wasted the attempt.
/// Capped so a user-configured long hold cannot escalate past 2 s.
fn retry_hold_ms(base: u32, attempt: u32) -> u32 {
    base.saturating_mul(attempt + 2).min(2_000)
}

/// Bound a plan's opening sleep by `cap` (0 = uncapped, matching the
/// `no_budget_cap_ms` config semantics) and credit `elapsed_ms` already
/// spent since the decision's trigger. Only the first step is touched —
/// it is the thinking delay; later steps are inter-action gaps.
fn clamp_first_sleep(plan: &mut crate::autoplay::PlanResult, cap: u32, elapsed_ms: u32) {
    let cap = if cap == 0 { u32::MAX } else { cap };
    if let Some(Step::Sleep { duration_ms }) = plan.steps.first_mut() {
        *duration_ms = (*duration_ms).min(cap).saturating_sub(elapsed_ms);
    }
}

/// How long the galaxy queue may run empty before the sun fallback is
/// added (matching the client's "find a sun table" 2-minute timer).
const GALAXY_FALLBACK_AFTER: Duration = Duration::from_secs(120);

/// How long a queue attempt waits for a table before the session gives
/// up (finding one in the higher rooms routinely takes minutes).
const QUEUE_TIMEOUT: Duration = Duration::from_secs(600);

/// Queue the first match of a session immediately — called from the
/// session-start IPC when no game is in progress.
pub fn spawn_queue_task(
    cfg: Arc<RwLock<AppConfig>>,
    inject: crate::autoplay::inject::SharedInjectBus,
    session: crate::autoplay::session::SharedAutoplaySession,
    notify: NotifyBus,
    config_dir: std::path::PathBuf,
) {
    tauri::async_runtime::spawn(async move {
        run_queue_task(cfg, inject, session, notify, config_dir).await;
    });
}

async fn run_queue_task(
    cfg: Arc<RwLock<AppConfig>>,
    inject: crate::autoplay::inject::SharedInjectBus,
    session: crate::autoplay::session::SharedAutoplaySession,
    notify: NotifyBus,
    config_dir: std::path::PathBuf,
) {
    if let Err(reason) = queue_match(&cfg, &inject, &session, &notify, &config_dir).await
    {
        session.stop(&reason);
        warn!("autoplay session: {reason}");
        let _ = notify.send(
            crate::schema::Notification::error("Autoplay session stopped").body(reason),
        );
    }
}

/// Book one ranked match over the lobby HTTP API and wait for the table.
/// One attempt: on timeout the queues are left and the session stops. An
/// `Err` stops the session with that reason; `Ok(())` means a match
/// started (or the session was already over).
async fn queue_match(
    cfg: &Arc<RwLock<AppConfig>>,
    inject: &crate::autoplay::inject::SharedInjectBus,
    session: &crate::autoplay::session::SharedAutoplaySession,
    notify: &NotifyBus,
    config_dir: &std::path::Path,
) -> Result<(), String> {
    use crate::bridge::riichi_city::lobby;

    if !session.is_active() {
        return Ok(());
    }
    let rc = cfg.read().await.autoplay.riichi_city.clone();
    let deviceid = lobby::load_or_create_device_id(config_dir)
        .map_err(|e| format!("could not persist the lobby device id: {e:#}"))?;
    let creds = inject.lobby_credentials().ok_or_else(|| {
        "not connected — log into Riichi City with capture running first".to_string()
    })?;
    let client = lobby::LobbyClient::new(
        rc.queue_web_base
            .as_deref()
            .unwrap_or(lobby::DEFAULT_WEB_BASE),
        lobby::LobbyAuth::from_credentials(&creds, deviceid, rc.channel.clone()),
    );
    let classifies = client
        .read_classifies()
        .await
        .map_err(|e| format!("could not read the ranked-room list: {e:#}"))?;
    let fallback_classify = if rc.room == crate::config::RiichiRoom::Galaxy
        && rc.galaxy_fallback_sun
    {
        lobby::select_classify(&classifies, crate::config::RiichiRoom::Sun, rc.game_type)
            .cloned()
    } else {
        None
    };

    // The generation snapshot is the "a match started" signal: the
    // StartGame handler bumps it when the booked table actually opens.
    let generation = session.start_generation();
    let classify =
        lobby::select_classify(&classifies, rc.room, rc.game_type).ok_or_else(|| {
            "the configured room / game length is not offered right now".to_string()
        })?;
    let env = client
        .start_stage(&classify.id)
        .await
        .map_err(|e| format!("the queue request failed: {e:#}"))?;
    if env.code != 0 {
        // Never retried: the interesting refusals (identity challenge,
        // AI-ban notice, AFK penalty) all mean a human must act.
        return Err(format!(
            "queueing was refused: {}",
            lobby::start_stage_failure_reason(env.code)
        ));
    }
    info!("autoplay session: queued a match");
    session.note_queuing();
    let _ = notify.send(
        crate::schema::Notification::info("Autoplay session")
            .body("Queued for a match — waiting for a table."),
    );
    let queued_at = Instant::now();
    let deadline = queued_at + QUEUE_TIMEOUT;
    let mut fell_back = false;
    loop {
        if session.start_generation() != generation {
            session.clear_queue_wait();
            let _ = notify.send(
                crate::schema::Notification::info("Autoplay session")
                    .body("Match found — a new game is starting."),
            );
            return Ok(()); // matched — the new game drives itself
        }
        if !session.is_active() {
            session.clear_queue_wait();
            leave_queue(&client, inject).await;
            return Ok(());
        }
        if !fell_back {
            if let Some(sun) = &fallback_classify {
                if queued_at.elapsed() >= GALAXY_FALLBACK_AFTER {
                    // Additive like the client: sun is booked alongside
                    // galaxy — whichever table forms first wins. One
                    // attempt; a refused add leaves the galaxy queue
                    // running.
                    info!(
                        "autoplay session: no galaxy table in {}s — \
                         also queueing sun",
                        GALAXY_FALLBACK_AFTER.as_secs()
                    );
                    fell_back = true;
                    match client.start_stage(&sun.id).await {
                        Ok(env) if env.code == 0 => {
                            let _ = notify.send(
                                crate::schema::Notification::info("Autoplay session").body(
                                    "No galaxy table in 2 minutes — now also queueing sun.",
                                ),
                            );
                        }
                        Ok(env) => warn!(
                            "autoplay session: sun queue add refused (code {}) — \
                             staying in the galaxy queue",
                            env.code
                        ),
                        Err(e) => warn!(
                            "autoplay session: sun queue add failed: {e:#} — \
                             staying in the galaxy queue"
                        ),
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1_000)).await;
    }
    warn!(
        "autoplay session: no game within {}s — stopping",
        QUEUE_TIMEOUT.as_secs()
    );
    session.clear_queue_wait();
    leave_queue(&client, inject).await;
    Err(format!(
        "no match was found within {} minutes — session stopped",
        QUEUE_TIMEOUT.as_secs() / 60
    ))
}

/// Cancel every live queue (each by its own matchID; the fallback runs
/// two at once).
async fn leave_queue(
    client: &crate::bridge::riichi_city::lobby::LobbyClient,
    inject: &crate::autoplay::inject::SharedInjectBus,
) {
    for (_, match_id) in inject.queue_states() {
        match client.cancel_stage(&match_id).await {
            Ok(env) if env.code == 0 => info!("autoplay session: left a queue"),
            Ok(env) => warn!(
                "autoplay session: leaving a queue was refused (code {})",
                env.code
            ),
            Err(e) => warn!("autoplay session: leaving a queue failed: {e:#}"),
        }
    }
    inject.clear_queue_state();
}

async fn autoplay_enabled_for_riichi(cfg: &Arc<RwLock<AppConfig>>) -> bool {
    let guard = cfg.read().await;
    guard.autoplay.enabled && guard.platform.kind == crate::config::Platform::RiichiCity
}

/// Dedicated round-advance loop, on its own `MjaiBus` subscription so
/// end-of-hand plan backlogs cannot delay it. Advancing past the scoring
/// screen is one `req_user_prepare` per `EndKyoku`, sent only after the
/// client has actually rendered the OK button (sighted via
/// `vision::capture_ok_button`, once a second). No timed fallback: if the
/// button is never sighted, the server's own countdown advances the
/// table. `EndGame` needs nothing — the client tears its end screens down
/// when the next match's `cmd_enter_room` arrives.
async fn round_advance_watcher(
    cfg: Arc<RwLock<AppConfig>>,
    inject: crate::autoplay::inject::SharedInjectBus,
    bus: MjaiBus,
) {
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    /// Give up on sighting after this long — the client's own countdown is
    /// 59s, so anything past this means the window is gone or the detector
    /// is broken, and we must not fire blind.
    const SIGHTING_TIMEOUT: Duration = Duration::from_secs(90);

    let mut rx = bus.subscribe();
    loop {
        match rx.recv().await {
            Ok(ev) => match ev {
                MjaiEvent::EndKyoku => {
                    if !autoplay_enabled_for_riichi(&cfg).await {
                        continue;
                    }
                    let (_, yakus) = inject.settlement();
                    let mut ticker = tokio::time::interval(POLL_INTERVAL);
                    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                    let deadline = tokio::time::Instant::now() + SIGHTING_TIMEOUT;
                    let mut sighted = false;
                    let mut aborted = false;
                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                if tokio::time::Instant::now() >= deadline {
                                    break;
                                }
                                let hit = tauri::async_runtime::spawn_blocking(
                                    crate::autoplay::riichi_city::vision::capture_ok_button,
                                )
                                .await
                                .ok()
                                .flatten();
                                if let Some(sight) = hit {
                                    debug!(
                                        pixels = sight.pixels,
                                        box = ?(sight.x0, sight.y0, sight.x1, sight.y1),
                                        "autoplay: OK button sighted"
                                    );
                                    sighted = true;
                                    break;
                                }
                            }
                            msg = rx.recv() => match msg {
                                // The round moved on without us (server
                                // countdown, manual click) — void cycle.
                                Ok(_) => {
                                    aborted = true;
                                    break;
                                }
                                Err(RecvError::Lagged(_)) => continue,
                                Err(RecvError::Closed) => return,
                            }
                        }
                    }
                    if aborted || !sighted {
                        if !aborted {
                            warn!(
                                "autoplay: OK button never sighted within {}s; \
                                 not advancing this round",
                                SIGHTING_TIMEOUT.as_secs()
                            );
                        }
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(ok_button_beat_ms(yakus))).await;
                    info!("autoplay: sending round-advance (req_user_prepare)");
                    if !inject.send(InjectFrame {
                        gameplay: true,
                        bytes: crate::bridge::riichi_city::build::user_prepare(),
                    }) {
                        warn!("autoplay: no injection relay for the round-advance press");
                    }
                }
                MjaiEvent::EndGame { .. } => {}
                _ => {}
            },
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        }
    }
}

/// Reading beat between sighting the OK button and pressing it: the button
/// only renders once the breakdown is up, so this is pure viewing time,
/// scaled by the number of yaku lines in the winning hand (each line past
/// two adds half a second). Draws count as zero yakus.
fn ok_button_beat_ms(yakus: u32) -> u64 {
    const BASE: u64 = 1_500;
    const CAP: u64 = 4_000;
    (BASE + u64::from(yakus.saturating_sub(2)) * 500).min(CAP)
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
    /// `handle_mjai_event`. No CDP page, no config — just enough to
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

    /// Regression: `cached_our_seat` must be populated immediately when
    /// `StartGame` is received, before any bot response fires. Previously
    /// the seat was only cached inside `handle_bot_response`, so the first
    /// `Tsumo` event on the opening draw could arrive before the bot had
    /// responded and `last_self_tsumo` would be silently missed.
    #[test]
    fn start_game_sets_cached_seat_immediately() {
        let mut m = make_manager();

        // Before any event — no seat cached.
        assert!(m.state.cached_our_seat.is_none());

        // StartGame with id = Some(1) — seat must be cached right away.
        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(1),
            num_players: 4,
            majsoul_meta: None,
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
            majsoul_meta: None,
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
            majsoul_meta: None,
        });
        assert_eq!(m.state.dead_clicks, 0, "a new table is a fresh start");
    }

    /// A single-click plan (most of them: discard, pon, ron, skip) has
    /// nothing to escalate — every attempt presses the same one click.
    #[test]
    fn a_single_click_plan_always_retries_that_click() {
        let clicks = [(1.0, 2.0)];
        assert_eq!(retry_slice(&clicks, 0), &clicks[..]);
        assert_eq!(retry_slice(&clicks, 1), &clicks[..]);
    }

    /// A multi-click plan escalates: the committing click first (the one
    /// pressed while the candidate row was still animating), then the whole
    /// sequence. Re-pressing a chi button whose row is already open has an
    /// effect we cannot predict, so it is not the first thing tried.
    #[test]
    fn a_multi_click_plan_retries_the_committing_click_first() {
        let clicks = [(1.0, 2.0), (3.0, 4.0)];
        assert_eq!(
            retry_slice(&clicks, 0),
            &[(3.0, 4.0)],
            "first retry presses only the candidate/tile click"
        );
        assert_eq!(
            retry_slice(&clicks, 1),
            &clicks[..],
            "second retry assumes the opening click was lost too"
        );
    }

    /// Defensive: an empty plan is never verified, but the slice helper
    /// must not panic if it ever is.
    #[test]
    fn no_clicks_is_not_a_panic() {
        let clicks: [(f64, f64); 0] = [];
        assert!(retry_slice(&clicks, 0).is_empty());
        assert!(retry_slice(&clicks, 3).is_empty());
    }

    /// The first retry (attempt 0) is the *second* press overall and must
    /// already escalate the hold — `attempt + 1` would repeat the original
    /// hold verbatim and waste the attempt.
    #[test]
    fn retry_press_escalates_the_hold_from_the_first_retry() {
        assert_eq!(retry_hold_ms(120, 0), 240, "second press holds twice");
        assert_eq!(retry_hold_ms(120, 1), 360);
        assert_eq!(retry_hold_ms(1_500, 1), 2_000, "capped at 2s");
        assert_eq!(retry_hold_ms(u32::MAX, 5), 2_000, "no overflow");
    }

    /// A fat-tailed script sample is bounded by the no-budget cap, and time
    /// already spent since the trigger is credited — a queued decision
    /// shortens its own sleep instead of stacking.
    #[test]
    fn first_sleep_is_capped_and_credits_elapsed_queue_time() {
        use crate::autoplay::PlanResult;
        let mk = |ms: u32| PlanResult {
            steps: vec![Step::Sleep { duration_ms: ms }],
        };
        let mut p = mk(19_500);
        clamp_first_sleep(&mut p, 15_000, 6_300);
        assert_eq!(p.steps[0], Step::Sleep { duration_ms: 8_700 });

        // Small sample, no queue: untouched.
        let mut p = mk(2_000);
        clamp_first_sleep(&mut p, 15_000, 300);
        assert_eq!(p.steps[0], Step::Sleep { duration_ms: 1_700 });

        // Heavily queued: sleep gone, but never negative.
        let mut p = mk(3_000);
        clamp_first_sleep(&mut p, 15_000, 9_000);
        assert_eq!(p.steps[0], Step::Sleep { duration_ms: 0 });

        // cap 0 = uncapped: only elapsed is credited.
        let mut p = mk(19_500);
        clamp_first_sleep(&mut p, 0, 500);
        assert_eq!(p.steps[0], Step::Sleep { duration_ms: 19_000 });
    }

    /// Draws and ≤2-yaku hands get the flat base; each yaku line past two
    /// adds half a second; capped.
    #[test]
    fn ok_button_beat_scales_with_yaku_count_only() {
        assert_eq!(ok_button_beat_ms(0), 1_500, "draw");
        assert_eq!(ok_button_beat_ms(1), 1_500, "one yaku");
        assert_eq!(ok_button_beat_ms(2), 1_500, "two yaku lines");
        assert_eq!(ok_button_beat_ms(3), 2_000, "one extra line");
        assert_eq!(ok_button_beat_ms(5), 3_000, "three extra lines");
        assert_eq!(ok_button_beat_ms(9), 4_000, "capped");
    }

    /// Regression (stale discard into a live board): the client's discard
    /// handler applies locally even when the turn is over, so a discard must
    /// be dropped once the window it was planned against is gone. The riichi
    /// plan is exempt — its own button press is what replaces the window,
    /// and the tile is still owed.
    #[test]
    fn a_discard_is_guarded_by_its_window_except_for_riichi() {
        let dahai = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: false,
        };
        assert!(discard_needs_window_guard(&dahai));
        let reach = MjaiEvent::Reach {
            actor: 0,
            pai: Some("2m".into()),
        };
        assert!(!discard_needs_window_guard(&reach));
    }

    /// The window's `opened_at` is its identity: a slot holding a different
    /// instant — or nothing — means the plan is stale. No planned window
    /// (other platforms) never reports movement.
    #[test]
    fn tenhou_window_moved_tracks_the_slot() {
        use crate::autoplay::tenhou_state::{DecisionWindow, TenhouState};
        let m = make_manager();
        let w1 = DecisionWindow {
            ops: 0,
            opened_at: std::time::Instant::now(),
        };
        let put = |window| {
            *m.ctx.tenhou_state.write().unwrap() = Some(TenhouState {
                seat: 0,
                hand: vec![0],
                melds: Vec::new(),
                is_tsumo: true,
                window,
            });
        };

        put(Some(w1));
        assert!(
            !m.tenhou_window_moved(Some(w1)),
            "same instant — still live"
        );
        assert!(
            !m.tenhou_window_moved(None),
            "nothing planned, nothing stale"
        );

        put(None);
        assert!(m.tenhou_window_moved(Some(w1)), "window resolved — stale");

        let w2 = DecisionWindow {
            ops: 8,
            opened_at: std::time::Instant::now(),
        };
        put(Some(w2));
        assert!(m.tenhou_window_moved(Some(w1)), "window replaced — stale");
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
            majsoul_meta: None,
        });
        assert_eq!(m.state.cached_our_seat, Some(0));

        // New game, observer mode — no seat.
        m.handle_mjai_event(&MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: None,
            num_players: 4,
            majsoul_meta: None,
        });
        assert!(
            m.state.cached_our_seat.is_none(),
            "stale seat must be cleared"
        );
    }
}
