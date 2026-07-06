//! Lifecycle + decision-point batching for the active bot.
//!
//! `BotManager` owns one `Box<dyn BotRunner>`, subscribes to the
//! `MjaiBus`, accumulates events between decision points, and broadcasts
//! every `BotResponse` (including `MjaiEvent::None`) onto a
//! `BotResponseBus` for downstream consumers (HUD, storage, external WS).
//!
//! Decision-point policy intentionally errs wide: any event where the
//! bot *might* be allowed to act flushes the pending batch. The bot
//! itself returns `MjaiEvent::None` when no action is owed, so we can't
//! be wrong about "allowed to act" — only about whether the round-trip
//! to the bot is worth the latency. Currently we accept that latency.
//!
//! ## Status & notification emission
//!
//! Every lifecycle transition is published to two side-channel buses for
//! the IPC layer:
//!
//! - `BotStatusBus` — typed state machine
//!   (`Idle/Loading/Ready/Error/Stopped`). The frontend renders a spinner
//!   on `Loading{SyncingDeps}` so the user knows the slow first-run
//!   `uv sync` is in progress, not a hang.
//! - `NotifyBus` — toast-style notifications. Loading and error events
//!   reuse the same `id` (`"bot-loading-<name>"`) so the sticky
//!   "preparing" toast is replaced rather than duplicated when the spawn
//!   resolves.

use crate::bot::manifest;
use crate::bot::registry::BotRegistry;
use crate::bot::runner::{BotRunner, SubprocessBot};
use crate::bot::runtime::PythonRuntime;
use crate::bot::sync_guard::SyncGuard;
use crate::config::AppConfig;
use crate::event_bus::{BotResponseBus, BotStatusBus, NotifyBus};
use crate::inspector::InspectorWriter;
use crate::schema::{BotReaction, BotStatus, InspectorEntry, LoadStage, MjaiEvent, Notification};
use anyhow::{bail, Context, Result};
use chrono::Local;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, warn};

pub struct BotManager {
    /// Python runtime for `mjai_bot/*` subprocess bots. `None` when no
    /// python3+uv runtime was found — the built-in native bot still works;
    /// only Python subprocess bots require it (enforced per-spawn).
    runtime: Option<PythonRuntime>,
    /// Resolved root for `mjai_bot/`. Re-scanned on every `spawn_runner`
    /// so freshly installed bots (e.g. via the Setup wizard or the
    /// Install-from-GitHub button) are picked up without restarting
    /// Akagi — the manager's view of "what bots exist" must not be a
    /// snapshot taken at supervisor start.
    bot_dir: PathBuf,
    /// Shared, live application config. The active-bot selection
    /// (`bot.active_4p` / `bot.active_3p`) is read *fresh* at every
    /// `start_game` rather than snapshotted at construction, so a runtime
    /// switch via the Bots page (`set_active_bot`) or Settings
    /// (`update_config`) takes effect on the next game without relaunching
    /// Akagi.
    config: Arc<RwLock<AppConfig>>,
    /// Subdir name of the bot currently spawned for the in-progress game
    /// (the `active_4p` / `active_3p` value read from `config` at
    /// `start_game`). Empty until the first `start_game`.
    active_name: String,
    /// Player count of the in-progress game (from `start_game.num_players`).
    /// Used to construct the built-in native bot for the right mode.
    game_num_players: u8,
    runner: Option<Box<dyn BotRunner>>,
    /// Events seen since the last `react()` call.
    pending: Vec<MjaiEvent>,
    /// Bot's seat in the current game; set on `start_game`.
    actor_id: Option<u8>,
    out_tx: BotResponseBus,
    status_tx: BotStatusBus,
    notify_tx: NotifyBus,
    /// Inspector writer — one BotReaction record per `react()` call, so
    /// the Logs → Inspector tab can replay "trigger event → bot action"
    /// pairings without grepping multiple files.
    inspector: InspectorWriter,
    /// Shared with the IPC layer so a user-triggered Reinstall environment
    /// and an in-flight game-start sync can't run `uv sync` against the same
    /// venv simultaneously.
    syncs_in_flight: Arc<Mutex<HashSet<String>>>,
}

impl BotManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Option<PythonRuntime>,
        bot_dir: PathBuf,
        config: Arc<RwLock<AppConfig>>,
        out_tx: BotResponseBus,
        status_tx: BotStatusBus,
        notify_tx: NotifyBus,
        inspector: InspectorWriter,
        syncs_in_flight: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        Self {
            runtime,
            bot_dir,
            config,
            active_name: String::new(),
            game_num_players: 4,
            runner: None,
            pending: Vec::new(),
            actor_id: None,
            out_tx,
            status_tx,
            notify_tx,
            inspector,
            syncs_in_flight,
        }
    }

    pub fn out_tx(&self) -> &BotResponseBus {
        &self.out_tx
    }

    /// Block on the MJAI receiver, dispatching every event through `handle`.
    /// Returns when the channel is closed (all senders dropped).
    ///
    /// Caller subscribes via `mjai.subscribe()` rather than passing the
    /// `Sender` so the manager doesn't keep the channel alive itself —
    /// makes shutdown deterministic when the proxy stops producing.
    pub async fn run(mut self, mut rx: broadcast::Receiver<MjaiEvent>) -> Result<()> {
        info!("bot manager subscribed to MJAI bus; waiting for start_game (active bot is read from config at each start_game)");
        // Surface the initial state to any IPC consumer that subscribes
        // late. Send is no-op when no subscribers exist yet.
        self.emit_status(BotStatus::Idle);
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Err(e) = self.handle(ev).await {
                        error!("bot manager: {e:#}");
                        // Tear the runner down; next start_game will respawn.
                        self.runner = None;
                        self.pending.clear();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("bot manager lagged behind MJAI bus by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("MJAI bus closed; bot manager exiting");
                    return Ok(());
                }
            }
        }
    }

    /// Drive one event through the manager. Public for unit tests.
    pub async fn handle(&mut self, event: MjaiEvent) -> Result<()> {
        // Spawn the runner the moment we see the bot's seat in start_game.
        if let MjaiEvent::StartGame {
            id: Some(seat),
            num_players,
            ..
        } = &event
        {
            self.actor_id = Some(*seat);
            self.game_num_players = *num_players;
            // Pick the active bot for this game's player count, reading the
            // *current* config so a runtime model switch takes effect on the
            // next game (the manager outlives many games — a snapshot taken
            // at construction would pin the startup selection forever).
            let chosen = self
                .config
                .read()
                .await
                .bot
                .active_for(*num_players)
                .to_string();
            if chosen.is_empty() {
                warn!(
                    "no bot configured for {np}p; running analysis-only for this game",
                    np = num_players
                );
                self.runner = None;
                self.pending.clear();
                self.emit_status(BotStatus::Idle);
                return Ok(());
            }
            self.active_name = chosen;
            self.spawn_runner().await?;
            self.pending.clear();
        }

        // No runner means we don't even have a seat yet (no start_game with
        // `id` seen). Drop the event silently — we have no one to feed.
        if self.runner.is_none() {
            return Ok(());
        }

        self.pending.push(event.clone());

        if !self.is_decision_point(&event) {
            return Ok(());
        }

        let runner = self
            .runner
            .as_mut()
            .expect("runner is Some — checked above");
        let batch = std::mem::take(&mut self.pending);
        let started = Instant::now();
        let resp = match runner.react(&batch).await {
            Ok(r) => r,
            Err(e) => {
                let err_str = format!("{e:#}");
                let bot = self.active_name.clone();
                self.emit_status(BotStatus::Error {
                    bot: bot.clone(),
                    error: err_str.clone(),
                });
                self.emit_notify(Notification::error("Bot reaction failed").body(err_str));
                return Err(e).context("bot react failed");
            }
        };
        let reaction_ms = started.elapsed().as_millis() as u64;
        debug!(action = ?resp.action, meta = ?resp.meta, reaction_ms, "bot reacted");
        // Inspector record: pair the trigger event (the last item in the
        // batch is the one that crossed the decision-point threshold)
        // with the bot's response, plus reaction latency. `MjaiEvent::None`
        // is still recorded so the timeline shows "the bot saw this and
        // chose not to act" — that's exactly the kind of edge case the
        // inspector exists for.
        if let Some(actor_id) = self.actor_id {
            if let Some(trigger) = batch.last().cloned() {
                self.inspector.record(InspectorEntry::BotReaction {
                    ts_ms: Local::now().timestamp_millis(),
                    reaction: BotReaction {
                        bot: self.active_name.clone(),
                        actor_id,
                        trigger,
                        action: resp.action.clone(),
                        meta: resp.meta.clone(),
                        reaction_ms,
                    },
                });
            }
        }
        // MjaiEvent::None still goes on the bus — downstream consumers
        // decide whether to render. Centralizes the "skip" decision.
        let _ = self.out_tx.send(resp);

        if matches!(event, MjaiEvent::EndGame) {
            // Drain runner cleanly (writes end_game to stdin internally
            // through the next reset on the next start_game). Drop it
            // here so resources release immediately.
            let bot = self.active_name.clone();
            self.runner = None;
            self.actor_id = None;
            self.emit_status(BotStatus::Stopped { bot });
        }
        Ok(())
    }

    /// Two-phase spawn so the IPC layer can show a "Syncing deps…" spinner
    /// during the slow first-run path before the subprocess actually
    /// starts. Each branch publishes status + notification before
    /// returning so the UI never sees a stuck `Loading` state.
    async fn spawn_runner(&mut self) -> Result<()> {
        let bot_name = self.active_name.clone();
        let actor_id = self
            .actor_id
            .context("spawn_runner called without actor_id")?;

        // Built-in native bots (pure Rust, no Python) bypass the registry /
        // venv path entirely: no `bot.py`, no `uv sync`, weights are embedded.
        // The runner is either the offline local model or, when the user opted
        // into cloud inference (`bot.api`), the API-backed one (which still
        // falls back to the local model). Read the config fresh so a toggle via
        // Settings takes effect on the next game.
        if crate::bot::native::is_native(&bot_name) {
            let api = self.config.read().await.bot.api.clone();
            let api_backed = api.is_active();
            match crate::bot::native::build(
                actor_id,
                self.game_num_players,
                &api,
                self.notify_tx.clone(),
            ) {
                Ok(runner) => {
                    info!(
                        bot = %bot_name,
                        actor_id,
                        num_players = self.game_num_players,
                        api_backed,
                        "native bot runner constructed"
                    );
                    self.emit_status(BotStatus::Ready {
                        bot: bot_name.clone(),
                        actor_id,
                    });
                    self.runner = Some(runner);
                    return Ok(());
                }
                Err(e) => {
                    let msg = format!("native bot init failed: {e:#}");
                    self.fail_load(&bot_name, &msg, "Built-in bot failed to load");
                    bail!(msg);
                }
            }
        }

        // Rescan on each spawn so bots installed after the supervisor
        // started (Setup wizard, Install-from-GitHub) are visible. A
        // snapshot taken at supervisor-start time misses them and the
        // user sees "bot not found" until they relaunch Akagi.
        let registry = match BotRegistry::scan(&self.bot_dir) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("scan {}: {e:#}", self.bot_dir.display());
                self.fail_load(&bot_name, &msg, "Bot directory unreadable");
                bail!(msg);
            }
        };
        let entry = match registry.find(&bot_name) {
            Some(e) => e.clone(),
            None => {
                let msg = format!(
                    "bot {:?} not found in registry at {}",
                    bot_name,
                    registry.root().display()
                );
                self.fail_load(&bot_name, &msg, "Bot not found");
                bail!(msg);
            }
        };
        if entry.pyproject.is_none() {
            let msg = format!(
                "bot {} has no pyproject.toml — required for uv sync",
                entry.name
            );
            self.fail_load(&bot_name, &msg, "Bot misconfigured");
            bail!(msg);
        }

        // Game-start must never run a slow `uv sync` inline: it would stall
        // the live game while the bot misses its turns (the historical
        // game-start-timeout bug, otherwise prevented by `set_active_bot`
        // refusing to activate a bot whose env isn't pre-installed). A
        // moved/renamed Akagi folder can silently invalidate an *already
        // active* bot's venv in a way only a full re-sync can repair —
        // Windows bakes the base-python path into the `Scripts/python.exe`
        // trampoline, so it can't be repointed in place the way `ensure_synced`
        // repoints the Unix symlink. Detect that here and fall back to
        // analysis-only with a reinstall prompt instead of blocking the game;
        // the user repairs it out-of-band (Bots page → Install environment)
        // and the next game spawns cleanly. Returning Ok (not bail) leaves the
        // runner unset so this game simply runs analysis-only.
        if crate::bot::runtime::needs_out_of_band_resync(&entry.dir) {
            let msg = format!(
                "{bot_name}'s Python environment needs reinstalling — the Akagi \
                 folder was moved or renamed. Open the Bots page and click \
                 Install environment."
            );
            warn!(bot = %bot_name, "{msg}");
            self.emit_status(BotStatus::Error {
                bot: bot_name.clone(),
                error: msg.clone(),
            });
            self.emit_notify(Notification::error("Bot environment needs reinstalling").body(msg));
            self.runner = None;
            self.pending.clear();
            return Ok(());
        }

        // From here on we spawn a Python subprocess bot, which requires the
        // runtime. A missing runtime fails just this spawn (analysis-only for
        // this bot); the built-in native bot was already handled above and
        // never reaches here.
        let runtime = match self.runtime.clone() {
            Some(rt) => rt,
            None => {
                let msg = format!(
                    "bot {bot_name} needs a python3+uv runtime, but none was found. \
                     Use the built-in bot, or install a Python runtime."
                );
                self.fail_load(&bot_name, &msg, "No Python runtime");
                bail!(msg);
            }
        };

        let load_id = format!("bot-loading-{bot_name}");

        // Phase 1: dep sync. ensure_synced is a no-op when stamp matches,
        // so the SyncingDeps state is brief on warm boots.
        self.emit_status(BotStatus::Loading {
            bot: bot_name.clone(),
            stage: LoadStage::SyncingDeps,
        });
        self.emit_notify(
            Notification::info("Preparing bot")
                .body("Installing Python dependencies — first launch may take a while.")
                .sticky()
                .id(load_id.clone()),
        );

        // Acquire the per-bot sync lock so a Reinstall-environment IPC
        // call (or any other in-flight sync) doesn't race us against the
        // same venv.
        let sync_guard = match SyncGuard::acquire(&self.syncs_in_flight, &bot_name).await {
            Some(g) => g,
            None => {
                let msg = format!("sync already in progress for {bot_name}");
                self.emit_status(BotStatus::Error {
                    bot: bot_name.clone(),
                    error: msg.clone(),
                });
                self.emit_notify(
                    Notification::error("Bot dependency install failed")
                        .body(msg.clone())
                        .id(load_id.clone()),
                );
                bail!(msg);
            }
        };

        let sync_result = runtime.ensure_synced(&entry.dir).await;
        drop(sync_guard);
        if let Err(e) = sync_result {
            let msg = format!("uv sync failed: {e:#}");
            self.emit_status(BotStatus::Error {
                bot: bot_name.clone(),
                error: msg.clone(),
            });
            self.emit_notify(
                Notification::error("Bot dependency install failed")
                    .body(msg)
                    .id(load_id),
            );
            return Err(e).context("ensure_synced");
        }

        // Phase 2: subprocess spawn.
        self.emit_status(BotStatus::Loading {
            bot: bot_name.clone(),
            stage: LoadStage::Spawning,
        });

        let mut cmd = runtime.command_for(&entry.dir, &["bot.py"]);
        cmd.arg(actor_id.to_string());

        // If the bot ships a manifest, resolve user values + manifest
        // defaults and hand the path to the resolved JSON over to the
        // child via AKAGI_BOT_CONFIG. Bots without a manifest get no env
        // var — same behaviour as v3 before settings existed.
        if let Some(m) = entry.manifest.as_ref() {
            match manifest::load_values(&entry.dir, m)
                .and_then(|values| manifest::write_resolved(&entry.dir, &values))
            {
                Ok(path) => {
                    cmd.env("AKAGI_BOT_CONFIG", &path);
                }
                Err(e) => {
                    let msg = format!("resolve bot settings: {e:#}");
                    self.emit_status(BotStatus::Error {
                        bot: bot_name.clone(),
                        error: msg.clone(),
                    });
                    self.emit_notify(
                        Notification::error("Bot settings resolution failed")
                            .body(msg)
                            .id(load_id),
                    );
                    return Err(e).context("resolve bot settings");
                }
            }
        }
        let bot = match SubprocessBot::spawn_with_command(
            cmd,
            runtime.clone(),
            &entry.dir,
            actor_id,
            self.notify_tx.clone(),
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("subprocess spawn failed: {e:#}");
                self.emit_status(BotStatus::Error {
                    bot: bot_name.clone(),
                    error: msg.clone(),
                });
                self.emit_notify(
                    Notification::error("Bot subprocess failed to start")
                        .body(msg)
                        .id(load_id),
                );
                return Err(e);
            }
        };

        info!(bot = %bot_name, actor_id, "bot runner spawned");
        self.emit_status(BotStatus::Ready {
            bot: bot_name.clone(),
            actor_id,
        });
        // Reuse the loading id so the sticky toast is replaced, not
        // duplicated. Frontend treats same-id as a swap.
        self.emit_notify(Notification::success(format!("{bot_name} ready")).id(load_id));
        self.runner = Some(Box::new(bot));
        Ok(())
    }

    fn fail_load(&self, bot: &str, error: &str, title: &str) {
        self.emit_status(BotStatus::Error {
            bot: bot.into(),
            error: error.into(),
        });
        self.emit_notify(Notification::error(title.to_owned()).body(error.to_owned()));
    }

    fn emit_status(&self, s: BotStatus) {
        let _ = self.status_tx.send(s);
    }

    fn emit_notify(&self, n: Notification) {
        let _ = self.notify_tx.send(n);
    }

    /// Conservative: every event that *might* be a decision point flushes
    /// the pending batch. Bot returns `MjaiEvent::None` when not its turn.
    fn is_decision_point(&self, e: &MjaiEvent) -> bool {
        let Some(me) = self.actor_id else {
            return false;
        };
        match e {
            // Own draws — bot decides discard / riichi / agari / kan.
            MjaiEvent::Tsumo { actor, .. } => *actor == me,
            // Others' calls / discards may open a chi/pon/kan/ron window.
            MjaiEvent::Dahai { actor, .. } => *actor != me,
            MjaiEvent::Kakan { actor, .. } => *actor != me,
            // Own calls without a following rinshan tsumo: bot must pick
            // the post-call discard. ankan/kakan get a rinshan Tsumo that
            // flushes anyway, so they stay out of this set.
            MjaiEvent::Chi { actor, .. }
            | MjaiEvent::Pon { actor, .. }
            | MjaiEvent::Daiminkan { actor, .. } => *actor == me,
            // Round / game boundaries: bot may want to flush state.
            MjaiEvent::ReachAccepted { .. }
            | MjaiEvent::Hora { .. }
            | MjaiEvent::Ryukyoku { .. }
            | MjaiEvent::EndKyoku
            | MjaiEvent::EndGame => true,
            // Everything else (start_game/start_kyoku, our own dahai,
            // ankan/kakan, dora reveal, reach declaration) accumulates
            // without bothering the bot — its state catches up the next
            // time we flush.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::types::BotResponse;
    use crate::event_bus::{bot_response_bus, bot_status_bus, notify_bus};
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Records every `react` batch and replies with a scripted action.
    #[derive(Default)]
    struct MockBotRunner {
        calls: Arc<Mutex<Vec<Vec<MjaiEvent>>>>,
        next: Arc<Mutex<Vec<BotResponse>>>,
        /// If set, react() returns this error instead of consuming `next`.
        fail_with: Arc<Mutex<Option<String>>>,
    }

    impl MockBotRunner {
        fn new(replies: Vec<BotResponse>) -> (Self, Arc<Mutex<Vec<Vec<MjaiEvent>>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let r = Self {
                calls: calls.clone(),
                next: Arc::new(Mutex::new(replies)),
                fail_with: Arc::new(Mutex::new(None)),
            };
            (r, calls)
        }

        fn failing(err: &str) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                next: Arc::new(Mutex::new(Vec::new())),
                fail_with: Arc::new(Mutex::new(Some(err.into()))),
            }
        }
    }

    #[async_trait]
    impl BotRunner for MockBotRunner {
        async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
            self.calls.lock().await.push(events.to_vec());
            if let Some(err) = self.fail_with.lock().await.as_deref() {
                bail!(err.to_string());
            }
            let mut q = self.next.lock().await;
            if q.is_empty() {
                Ok(BotResponse {
                    action: MjaiEvent::None,
                    meta: None,
                })
            } else {
                Ok(q.remove(0))
            }
        }
        async fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn dummy_runtime() -> PythonRuntime {
        PythonRuntime::from_paths(
            PathBuf::from("/dev/null/python"),
            PathBuf::from("/dev/null/uv"),
            crate::bot::runtime::RuntimeMode::System,
        )
    }

    /// Path that `BotRegistry::scan` resolves to an empty registry —
    /// `scan` treats a non-existent root as "no bots".
    fn empty_bot_dir() -> PathBuf {
        PathBuf::from("/nonexistent/akagi-test-bot-dir")
    }

    fn fresh_syncs() -> Arc<Mutex<HashSet<String>>> {
        Arc::new(Mutex::new(HashSet::new()))
    }

    /// Shared config handle pre-seeded with a 4p active bot (and no 3p bot),
    /// matching how the supervisor hands the manager its live config. The
    /// active-bot selection is read from this at every `start_game`.
    fn cfg_with(active_4p: &str) -> Arc<RwLock<AppConfig>> {
        let mut c = AppConfig::default();
        c.bot.active_4p = active_4p.to_string();
        c.bot.active_3p = String::new();
        Arc::new(RwLock::new(c))
    }

    /// Tempfile-backed inspector writer for tests. The file is leaked
    /// for the test duration (the OS reaps it on process exit) — keeps
    /// the constructor a one-liner without per-test cleanup boilerplate.
    fn dummy_inspector() -> InspectorWriter {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.into_temp_path().keep().unwrap();
        InspectorWriter::open(&path, 8).unwrap().0
    }

    // Test-only helper; the 5-tuple return groups the manager with the
    // listener handles its consumers need. Splitting it into a dedicated
    // struct just for clippy would obscure the test setup, so allow the
    // complexity here.
    #[allow(clippy::type_complexity)]
    fn manager_with_mock(
        replies: Vec<BotResponse>,
    ) -> (
        BotManager,
        Arc<Mutex<Vec<Vec<MjaiEvent>>>>,
        broadcast::Receiver<BotResponse>,
        broadcast::Receiver<BotStatus>,
        broadcast::Receiver<Notification>,
    ) {
        let (mock, calls) = MockBotRunner::new(replies);
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let resp_rx = bus.subscribe();
        let status_rx = status.subscribe();
        let notify_rx = notify.subscribe();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            cfg_with("mock"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );
        // Pre-seat the actor and inject the mock so we don't go through
        // the registry / runtime path (covered by runner.rs tests).
        // `start_game` normally sets `active_name` alongside the runner, so
        // mirror that invariant here for status emissions to be realistic.
        mgr.actor_id = Some(2);
        mgr.active_name = "mock".into();
        mgr.runner = Some(Box::new(mock));
        (mgr, calls, resp_rx, status_rx, notify_rx)
    }

    fn dahai(actor: u8) -> MjaiEvent {
        MjaiEvent::Dahai {
            actor,
            pai: "1m".into(),
            tsumogiri: false,
        }
    }

    #[tokio::test]
    async fn non_decision_events_accumulate_without_calling_react() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);

        // None of these are decision points for seat 2:
        //   - own dahai (seat 2)
        //   - own tsumo
        //   - dora reveal
        mgr.handle(MjaiEvent::Tsumo {
            actor: 0,
            pai: "1m".into(),
        })
        .await
        .unwrap(); // not our tsumo, but is also NOT in decision set
        mgr.handle(dahai(2)).await.unwrap(); // our own dahai
        mgr.handle(MjaiEvent::Dora {
            dora_marker: "5p".into(),
        })
        .await
        .unwrap();

        assert!(
            calls.lock().await.is_empty(),
            "no decision points → no react calls"
        );
        assert_eq!(mgr.pending.len(), 3, "events should be buffered");
    }

    #[tokio::test]
    async fn others_dahai_flushes_batch() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);
        mgr.handle(MjaiEvent::Dora {
            dora_marker: "5p".into(),
        })
        .await
        .unwrap();
        mgr.handle(dahai(0)).await.unwrap(); // someone else's dahai

        let calls = calls.lock().await;
        assert_eq!(calls.len(), 1, "exactly one react call");
        assert_eq!(calls[0].len(), 2, "batch carries the buffered + trigger");
        assert!(matches!(calls[0][0], MjaiEvent::Dora { .. }));
        assert!(matches!(calls[0][1], MjaiEvent::Dahai { actor: 0, .. }));
    }

    #[tokio::test]
    async fn own_tsumo_flushes_others_tsumo_does_not() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);

        // Others' tsumo: NOT a decision point.
        mgr.handle(MjaiEvent::Tsumo {
            actor: 0,
            pai: "1m".into(),
        })
        .await
        .unwrap();
        assert!(calls.lock().await.is_empty());

        // Our tsumo: IS a decision point.
        mgr.handle(MjaiEvent::Tsumo {
            actor: 2,
            pai: "5m".into(),
        })
        .await
        .unwrap();
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 1);
        // Both events flushed in the batch.
        assert_eq!(calls[0].len(), 2);
    }

    #[tokio::test]
    async fn own_pon_flushes_so_bot_picks_post_call_discard() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);

        // Others' dahai (actor 0) — flushes the batch as the call window.
        mgr.handle(dahai(0)).await.unwrap();
        assert_eq!(calls.lock().await.len(), 1);

        // Our pon: must also be a decision point so the bot returns the
        // post-call discard. Without the flush, manager would buffer it
        // forever — no rinshan tsumo follows pon.
        mgr.handle(MjaiEvent::Pon {
            actor: 2,
            target: 0,
            pai: "1m".into(),
            consumed: ["1m".into(), "1m".into()],
        })
        .await
        .unwrap();

        let calls = calls.lock().await;
        assert_eq!(calls.len(), 2, "own pon must trigger react()");
        assert!(matches!(
            calls[1].last().unwrap(),
            MjaiEvent::Pon { actor: 2, .. }
        ));
    }

    #[tokio::test]
    async fn own_chi_and_daiminkan_also_flush() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);

        mgr.handle(MjaiEvent::Chi {
            actor: 2,
            target: 1,
            pai: "3m".into(),
            consumed: ["4m".into(), "5m".into()],
        })
        .await
        .unwrap();
        assert_eq!(calls.lock().await.len(), 1, "own chi must flush");

        mgr.handle(MjaiEvent::Daiminkan {
            actor: 2,
            target: 0,
            pai: "5m".into(),
            consumed: ["5m".into(), "5m".into(), "5mr".into()],
        })
        .await
        .unwrap();
        assert_eq!(calls.lock().await.len(), 2, "own daiminkan must flush");
    }

    #[tokio::test]
    async fn others_pon_does_not_flush() {
        let (mut mgr, calls, _, _, _) = manager_with_mock(vec![]);
        mgr.handle(MjaiEvent::Pon {
            actor: 0,
            target: 3,
            pai: "1m".into(),
            consumed: ["1m".into(), "1m".into()],
        })
        .await
        .unwrap();
        assert!(
            calls.lock().await.is_empty(),
            "others' pon: bot has nothing to do, must not flush"
        );
    }

    #[tokio::test]
    async fn bot_response_broadcast_to_subscribers() {
        let scripted = BotResponse {
            action: dahai(2),
            meta: None,
        };
        let (mut mgr, _, mut rx, _, _) = manager_with_mock(vec![scripted.clone()]);
        mgr.handle(dahai(0)).await.unwrap(); // others' dahai → flush

        let received = rx.try_recv().expect("bot response should be broadcast");
        assert_eq!(received, scripted);
    }

    #[tokio::test]
    async fn end_game_flushes_drops_runner_emits_stopped() {
        let (mut mgr, calls, _, mut status_rx, _) = manager_with_mock(vec![]);
        mgr.handle(MjaiEvent::EndGame).await.unwrap();
        assert_eq!(calls.lock().await.len(), 1);
        assert!(mgr.runner.is_none());
        assert!(mgr.actor_id.is_none());

        let status = status_rx.try_recv().expect("status emitted");
        assert!(
            matches!(status, BotStatus::Stopped { .. }),
            "expected Stopped, got {status:?}"
        );
    }

    #[tokio::test]
    async fn react_failure_emits_error_status_and_notification() {
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut status_rx = status.subscribe();
        let mut notify_rx = notify.subscribe();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            cfg_with("mock"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );
        mgr.actor_id = Some(2);
        mgr.active_name = "mock".into();
        mgr.runner = Some(Box::new(MockBotRunner::failing("kaboom")));

        // Trigger a decision point — react() returns error.
        let err = mgr.handle(dahai(0)).await.unwrap_err();
        assert!(format!("{err:#}").contains("react failed"));

        let s = status_rx.try_recv().unwrap();
        match s {
            BotStatus::Error { bot, error } => {
                assert_eq!(bot, "mock");
                assert!(error.contains("kaboom"), "got error: {error}");
            }
            other => panic!("expected Error, got {other:?}"),
        }

        let n = notify_rx.try_recv().unwrap();
        assert_eq!(n.level, crate::schema::NotifyLevel::Error);
        assert!(n.title.contains("Bot reaction failed"));
    }

    #[tokio::test]
    async fn missing_bot_in_registry_emits_error_status() {
        // Empty registry + handle(StartGame{id}) → spawn_runner errors,
        // emits BotStatus::Error and a notification.
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut status_rx = status.subscribe();
        let mut notify_rx = notify.subscribe();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            cfg_with("ghost"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );

        let err = mgr
            .handle(MjaiEvent::StartGame {
                names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                kyoku_first: None,
                aka_flag: None,
                id: Some(0),
                num_players: 4,
            })
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("not found in registry"));

        let s = status_rx.try_recv().unwrap();
        assert!(
            matches!(s, BotStatus::Error { ref bot, .. } if bot == "ghost"),
            "expected Error{{bot=ghost}}, got {s:?}"
        );
        let n = notify_rx.try_recv().unwrap();
        assert_eq!(n.level, crate::schema::NotifyLevel::Error);
        assert!(n.title.contains("Bot not found"));
    }

    /// Regression (issue #157): switching the active bot after the manager
    /// is constructed must take effect on the next `start_game`. Pre-fix the
    /// manager snapshotted `active_4p`/`active_3p` at construction, so a
    /// runtime model switch via the Bots page was silently ignored until
    /// Akagi was relaunched.
    ///
    /// We can't run the full spawn (no real Python runtime in tests), so we
    /// lean on `spawn_runner`'s registry lookup: with an empty registry the
    /// spawn errors "bot not found", and the emitted `BotStatus::Error`
    /// carries the bot name the manager *tried* to spawn. Seeing the
    /// post-switch name there proves the manager re-read config at
    /// `start_game` rather than using the construction-time snapshot.
    #[tokio::test]
    async fn active_bot_switch_takes_effect_on_next_start_game() {
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut status_rx = status.subscribe();

        let config = cfg_with("old-bot");
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            config.clone(),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );

        // Switch the active bot AFTER construction, exactly as `set_active_bot`
        // does to the shared config while the manager task is already running.
        config.write().await.bot.active_4p = "new-bot".to_string();

        let err = mgr
            .handle(MjaiEvent::StartGame {
                names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                kyoku_first: None,
                aka_flag: None,
                id: Some(0),
                num_players: 4,
            })
            .await
            .unwrap_err();
        // The spawn must have been attempted for the *new* bot, not the
        // snapshot taken at construction.
        let msg = format!("{err:#}");
        assert!(msg.contains("new-bot"), "error should name new-bot: {msg}");
        assert!(!msg.contains("old-bot"), "must not use stale bot: {msg}");

        let s = status_rx.try_recv().unwrap();
        assert!(
            matches!(s, BotStatus::Error { ref bot, .. } if bot == "new-bot"),
            "expected Error{{bot=new-bot}}, got {s:?}"
        );
    }

    #[tokio::test]
    async fn events_before_start_game_are_dropped() {
        // Manager freshly constructed → no actor_id, no runner.
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            cfg_with("mock"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );
        // Should not panic / error even with no runner.
        mgr.handle(dahai(0)).await.unwrap();
        assert!(mgr.pending.is_empty());
    }

    /// Regression: a bot directory that gets populated *after* the
    /// `BotManager` is constructed must still be discoverable by the
    /// next `start_game`. Pre-fix the manager held a registry snapshot
    /// taken at supervisor-start time, so the Setup wizard's installs
    /// only became visible after a full Akagi relaunch — and game-start
    /// errored with "bot not found in registry".
    ///
    /// We can't run the full spawn flow (no real Python runtime in
    /// tests), so we lean on the second check inside `spawn_runner`:
    /// once the registry finds the entry, it errors with "no
    /// pyproject.toml" instead of "not found in registry". Hitting that
    /// second error proves the rescan happened.
    #[tokio::test]
    async fn registry_is_rescanned_on_each_start_game() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bot_dir = tmp.path().to_path_buf();

        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            bot_dir.clone(),
            cfg_with("latebot"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );

        // Drop a bot under bot_dir AFTER the manager exists. With the
        // old snapshot-at-construction behaviour, this would never be
        // visible to spawn_runner.
        let new_bot = bot_dir.join("latebot");
        std::fs::create_dir_all(&new_bot).unwrap();
        std::fs::write(new_bot.join("bot.py"), b"").unwrap();

        let err = mgr
            .handle(MjaiEvent::StartGame {
                names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                kyoku_first: None,
                aka_flag: None,
                id: Some(0),
                num_players: 4,
            })
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no pyproject.toml"),
            "expected the rescan to find latebot and fail at the pyproject \
             check; got: {msg}"
        );
        assert!(
            !msg.contains("not found in registry"),
            "registry rescan failed to pick up post-construction install: {msg}"
        );
    }

    /// Regression: an ACTIVE bot whose venv was invalidated by a folder move
    /// (the interpreter file survived but its base `home` is gone — the
    /// Windows shape, where `Scripts/python.exe` is a real trampoline copy)
    /// must NOT trigger an inline `uv sync` at game-start. That would stall
    /// the live game while the bot misses its turns — the historical
    /// game-start-timeout bug. `spawn_runner` detects the un-repointable venv
    /// up front and runs analysis-only with a reinstall prompt, never calling
    /// `ensure_synced`.
    #[tokio::test]
    async fn moved_venv_skips_inline_sync_and_runs_analysis_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bot_dir = tmp.path().to_path_buf();
        let bot = bot_dir.join("mybot");
        std::fs::create_dir_all(&bot).unwrap();
        std::fs::write(bot.join("bot.py"), b"").unwrap();
        std::fs::write(bot.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
        // Moved-venv state: interpreter file present, but pyvenv.cfg `home`
        // points at a directory that no longer exists.
        let venv = bot.join(".akagi").join("venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(venv.join("bin").join("python"), b"").unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            b"home = /vanished/old/runtime/bin\n",
        )
        .unwrap();

        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut status_rx = status.subscribe();
        let mut notify_rx = notify.subscribe();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            bot_dir,
            cfg_with("mybot"),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );

        mgr.handle(MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(0),
            num_players: 4,
        })
        .await
        .expect("handle returns Ok (analysis-only), not an error");

        assert!(mgr.runner.is_none(), "must not spawn a bot needing re-sync");

        // The guard fired BEFORE ensure_synced: an Error status naming the
        // bot with the reinstall message — NOT a 'uv sync failed' message,
        // which is what we'd see if the inline sync had been attempted.
        let s = status_rx.try_recv().expect("status emitted");
        match s {
            BotStatus::Error { bot, error } => {
                assert_eq!(bot, "mybot");
                assert!(
                    error.contains("reinstalling"),
                    "expected reinstall prompt, got: {error}"
                );
                assert!(
                    !error.contains("uv sync"),
                    "must not have attempted an inline sync: {error}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let n = notify_rx.try_recv().expect("notification emitted");
        assert_eq!(n.level, crate::schema::NotifyLevel::Error);
        assert!(n.title.contains("reinstalling"), "got title: {}", n.title);
    }

    /// Regression: the built-in native bot must spawn even when no python3+uv
    /// runtime is available (it needs none). Pre-fix the supervisor bailed on a
    /// missing runtime, so enabling the *default* native bot produced no
    /// reaction on machines without an Akagi Python runtime.
    #[tokio::test]
    async fn native_bot_spawns_without_python_runtime() {
        let bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut status_rx = status.subscribe();
        let mut mgr = BotManager::new(
            None, // no python runtime available
            empty_bot_dir(),
            cfg_with(crate::bot::native::NATIVE_4P),
            bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );

        mgr.handle(MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(0),
            num_players: 4,
        })
        .await
        .expect("native bot must spawn without a Python runtime");

        assert!(
            mgr.runner.is_some(),
            "native runner should be constructed even with runtime=None"
        );
        let s = status_rx.try_recv().expect("status emitted");
        assert!(
            matches!(s, BotStatus::Ready { ref bot, .. } if bot == crate::bot::native::NATIVE_4P),
            "expected Ready for the native bot, got {s:?}"
        );
    }

    #[tokio::test]
    async fn run_returns_ok_when_bus_closes() {
        // Subscribe outside the task so the task holds only the Receiver.
        // Dropping the Sender outside causes a clean Closed → Ok(()) exit.
        let mjai = crate::event_bus::mjai_bus();
        let rx = mjai.subscribe();

        let bot_bus = bot_response_bus();
        let status = bot_status_bus();
        let notify = notify_bus();
        let mut mgr = BotManager::new(
            Some(dummy_runtime()),
            empty_bot_dir(),
            cfg_with("mock"),
            bot_bus,
            status,
            notify,
            dummy_inspector(),
            fresh_syncs(),
        );
        mgr.actor_id = Some(2);
        let (mock, _calls) = MockBotRunner::new(vec![]);
        mgr.runner = Some(Box::new(mock));

        let handle = tokio::spawn(async move { mgr.run(rx).await });
        drop(mjai); // last sender → channel closes
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("manager exited")
            .expect("join")
            .expect("Ok");
    }
}
