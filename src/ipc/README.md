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
| `overlay-config`| `config::OverlayConfig`            | `overlay::reconcile` → every webview |

Every event above is broadcast to **all** webviews, not just the main one.
That is what lets the overlay window (see below) render suggestions off
`bot-response` without any plumbing of its own. `overlay-config` keeps the
main window's Game-page toggle in sync with persisted setting changes.

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
| `update_config`  | `new_config`          | `()`                     | Persists to TOML; subsystems do **not** auto-restart. Does reconcile the overlay window against `overlay.*` |
| `set_overlay_enabled` | `enabled`        | `()`                     | Flips + persists `overlay.enabled` and opens/closes the window for the Game-page toolbar shortcut |
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

## Windows

`overlay.rs` owns the app's second window: a frameless, transparent,
always-on-top card that floats over the game client and renders the bot's
top-N suggestions.

Four things are worth knowing before touching it:

- **Window creation must happen on the main thread.** On Windows,
  `WebviewWindowBuilder::build()` called from a Tokio worker — which is where
  every `#[tauri::command] async fn` body runs — **deadlocks the GUI**: it asks
  the event loop to create the window, then blocks the caller waiting for a
  reply the main thread cannot deliver. There is no panic, no error, and no log
  line; background tasks keep running, so the logs look healthy while the UI is
  frozen solid. `overlay::reconcile` therefore posts its work through
  `run_on_main_thread` and returns immediately. Anything you add that touches
  the window belongs inside `overlay::apply`, not in the command.
- **Both windows load the same `index.html`.** The frontend branches on
  `getCurrentWindow().label` (see `frontend/src/main.tsx`) to decide whether to
  mount the router or the overlay root. Window identity therefore lives in
  `overlay::LABEL` and nowhere else — no magic URLs to keep in sync.
- **The label needs a capability.** `capabilities/overlay.json` is scoped to
  `windows: ["overlay"]`. Rename `overlay::LABEL` without renaming that and the
  overlay webview silently loses permission to `listen()`, i.e. renders blank
  forever with no error.
- **`tauri-plugin-window-state` must skip it.** The plugin's automatic restore
  applies `StateFlags::all()`, which includes `DECORATIONS` — it would put a
  title bar back onto a deliberately frameless window. `lib.rs` therefore
  registers the plugin with `.skip_initial_state(ipc::overlay::LABEL)` and
  `overlay::open` restores position + size itself.

Lifecycle is driven entirely by `config.overlay.enabled` (default: **on**)
through `overlay::reconcile`, which is idempotent and called from three places:
app startup, `update_config`, and `set_overlay_enabled`. The last is what the
Game page's toolbar toggle calls. The overlay's own × closes only its current
window without changing `overlay.enabled`, so it returns on the next launch.

**The overlay must never outlive the app.** Tauri exits when *all* windows
close, and the overlay counts as one — so `lib.rs` hooks the main window's
`CloseRequested` and closes the overlay there, letting Tauri exit on its own
once no windows remain (rather than `exit(0)`, which would skip the
window-state save and the rest of the shutdown path). Skipping this is not a
cosmetic bug: the overlay is `skip_taskbar` and undecorated, so a lingering one
is a card floating over the desktop with no taskbar entry, no title bar, and
nothing that leads back to the headless process still running behind it (#192).

Closing the overlay's *window*, whether from its own × or during app shutdown,
must **not** touch `overlay.enabled`; only the toolbar or Settings toggle
persistently disables the feature.

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
