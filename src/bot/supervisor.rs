//! Spawns + drives a `BotManager`.
//!
//! Lives outside `lib::run` so the same entry point can be reused both at
//! startup (when `bot.enabled` is true on first config load) and at
//! runtime (when the user flips `bot.enabled` via the first-run wizard or
//! settings page — `update_config` calls this instead of forcing the user
//! to restart Akagi).

use crate::bot::registry::BotRegistry;
use crate::bot::runtime::PythonRuntime;
use crate::bot::BotManager;
use crate::config::AppConfig;
use crate::event_bus::{BotResponseBus, BotStatusBus, MjaiBus, NotifyBus};
use crate::inspector::InspectorWriter;
use crate::util;
use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

/// Build a `BotManager` from the shared `config` + `runtime` and run it
/// until the MJAI bus closes. Returns `Err` only on setup failure (missing
/// runtime, unscannable bot dir); transient runtime errors are absorbed by
/// the manager itself.
///
/// The manager holds the shared `Arc<RwLock<AppConfig>>` and re-reads the
/// active-bot selection at each `start_game`, so a model switch made while
/// the manager is running takes effect on the next game without a relaunch.
#[allow(clippy::too_many_arguments)]
pub async fn run_bot_manager(
    config: Arc<RwLock<AppConfig>>,
    mjai: MjaiBus,
    response_bus: BotResponseBus,
    status_bus: BotStatusBus,
    notify_bus: NotifyBus,
    inspector: InspectorWriter,
    runtime: Option<PythonRuntime>,
    syncs_in_flight: Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    let bot_dir = {
        let cfg = config.read().await;
        let bot_dir = util::resolve_dir(Path::new(&cfg.bot.dir));
        // Diagnostic-only: warn early if the configured bots aren't present
        // *now*. The manager rescans on every spawn so a bot installed after
        // this point is still picked up — the warning here is just to surface
        // mis-config quickly in logs, not to gate startup.
        let registry = BotRegistry::scan(&bot_dir)?;
        for (label, name) in [("4p", &cfg.bot.active_4p), ("3p", &cfg.bot.active_3p)] {
            if !name.is_empty() && registry.find(name).is_none() {
                warn!(
                    "configured {} bot {:?} not found under {}; available: {:?}",
                    label,
                    name,
                    bot_dir.display(),
                    registry.names().collect::<Vec<_>>()
                );
            }
        }
        bot_dir
    };

    let runtime = runtime
        .ok_or_else(|| anyhow!("bot mode is enabled but no python3+uv runtime was found"))?;

    let manager = BotManager::new(
        runtime,
        bot_dir,
        config,
        response_bus,
        status_bus,
        notify_bus,
        inspector,
        syncs_in_flight,
    );
    let rx = mjai.subscribe();
    manager.run(rx).await
}
