# `src/ipc` — Backend ↔ frontend bridge

Tauri integration layer. Exposes:

- **Commands** (frontend → backend) — invoked via Tauri's `invoke()`.
- **Events** (backend → frontend) — `app.emit()` from forwarder tasks
  that subscribe to the in-process broadcast buses in `crate::event_bus`.

This module is the *only* place that talks to Tauri's `AppHandle`. Other
subsystems (proxy, bot manager, bridge) stay UI-agnostic and emit via
buses.

## Wiring (already done in `src/lib.rs`)

```rust
let state = ipc::AppState::new(cfg, config_path, session, /*…buses…*/);

tauri::Builder::default()
    .invoke_handler(akagi::ipc_handlers!())
    .setup(move |app| {
        ipc::install(&app.handle(), state.clone())?;
        Ok(())
    })
```

`install()` does two things: `app.manage(state)` (so commands can read it
via `tauri::State<AppState>`) and spawns one forwarder task per bus.

## Events emitted to the frontend

| Event name      | Payload                            | Source                             |
|-----------------|------------------------------------|------------------------------------|
| `mjai-event`    | `schema::MjaiEvent`                | proxy bridge → `event_bus::MjaiBus` |
| `bot-response`  | `bot::BotResponse`                 | `BotManager` → `BotResponseBus`    |
| `bot-status`    | `schema::BotStatus`                | `BotManager` → `BotStatusBus`      |
| `proxy-status`  | `schema::ProxyStatus`              | `proxy_supervisor` → `ProxyStatusBus` |
| `notify`        | `schema::Notification`             | any subsystem → `NotifyBus`        |

Frontend subscribes once at app start:

```ts
import { listen } from "@tauri-apps/api/event";

await listen<BotStatus>("bot-status", e => store.setBotStatus(e.payload));
await listen<Notification>("notify", e => toast(e.payload));
```

Status buses are *also* mirrored into `AppState` snapshots so the
frontend can recover the current state on reload via `get_status`
without waiting for the next event.

## Commands callable from the frontend

| Command          | Args                  | Returns                  | Notes                        |
|------------------|-----------------------|--------------------------|------------------------------|
| `get_config`     | —                     | `AppConfig`              | Live read of in-memory config|
| `update_config`  | `new_config`          | `()`                     | Persists to TOML; subsystems do **not** auto-restart |
| `list_bots`      | —                     | `Vec<BotInfo>`           | Re-scans `cfg.bot.dir`       |
| `set_active_bot` | `mode, name`          | `()`                     | Updates + persists `bot.active_4p` or `bot.active_3p` (`mode` ∈ `"4p"` / `"3p"`); empty `name` clears the slot |
| `install_bot_from_github` | `repo, asset_glob?, name?` | `BotInfo`     | Download + extract; runs `uv sync` post-install if a runtime is available |
| `install_bot_from_zip` | `zip_path, name?`     | `BotInfo`                | Install from a local `.zip` (same extract/validate/`uv sync` pipeline as the GitHub install, minus the download). `name` defaults to the zip file stem; the source zip is never deleted |
| `update_bot_from_manifest` | `name`            | `BotInfo`                | Reinstall from the source declared in the bot's `manifest.toml` |
| `sync_bot_deps`  | `name, force`         | `()`                     | Re-runs `uv sync` for an installed bot. `force=true` wipes `.akagi/synced.stamp` and `.akagi/venv/` first (used by the per-bot Reinstall environment button). Per-bot `SyncGuard` rejects concurrent calls. |
| `delete_bot`     | `name`                | `()`                     | Refuses if the bot is the active 4p/3p; refuses paths that escape `bot.dir` |
| `start_proxy`    | —                     | `()` / `Err("…running")` | Spawns supervisor; idempotent guard |
| `stop_proxy`     | —                     | `()`                     | Sends shutdown to current proxy task |
| `get_status`     | —                     | `Snapshot`               | One-shot dump (config, bot_status, proxy_status, log_dir) |
| `get_log_dir`    | —                     | `PathBuf`                | Current log session directory|

Errors are returned as `String` so the frontend can put them straight
into a toast.

## Adding a new event

1. Define the payload in `crate::schema::ipc` (Serialize + Deserialize).
2. Add a bus type + constructor in `crate::event_bus`.
3. Plumb a clone of the `Sender` through `AppState`.
4. Add a `forward(...)` line in `mod.rs::spawn_forwarders` (or a custom
   forwarder if you also need to mirror state into a snapshot).
5. Document the event in this README.

## Adding a new command

1. Write `pub async fn …(state: State<'_, AppState>) -> Result<T, String>`
   in `commands.rs` with `#[tauri::command]`.
2. Add the function to the `ipc_handlers!()` macro in `commands.rs` so
   `tauri::generate_handler!` picks it up.
3. Document it in the table above.

## Testing strategy

- **Schema round-trips** live in `schema::ipc::tests` — proves the wire
  shape stays stable.
- **Command logic** — `commands::tests` covers persistence helpers.
  Tauri-injected `State<'_, AppState>` is hard to fake in unit tests;
  prefer extracting business logic into helper fns and testing those.
- **Bot lifecycle emission** — `bot::manager::tests` covers the error
  paths (`react_failure_emits_error_status_and_notification`,
  `missing_bot_in_registry_emits_error_status`,
  `end_game_flushes_drops_runner_emits_stopped`). The happy-path
  `Loading{SyncingDeps} → Loading{Spawning} → Ready` sequence is
  exercised end-to-end by the integration tests in `tests/example_bot.rs`.
