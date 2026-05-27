# Bot Module

Runs mjai-protocol AI bots and routes `MjaiEvent`s from the platform
bridge to them. 

## Submodules

- `types` — `BotResponse`. Wire shape matches mjai.app: an mjai action
  with an optional free-form `meta` JSON object for HUD-grade data
  (confidence, reasoning, q-values, …). Backend never interprets `meta`;
  the frontend renders it.
- `manifest` — per-bot `manifest.toml` schema + `settings.toml` values.
  See "Per-bot settings" below.
- `install` — fetch a release zip from GitHub, validate it, and drop it
  into `mjai_bot/<name>/`. See "Installing from GitHub" below.
- `registry` — discovers bot directories under `mjai_bot/`. No Python
  invocation; only filesystem layout. Populates `BotEntry.manifest` from
  any `manifest.toml` found in the bot directory.
- `runtime` — `PythonRuntime`: locates bundled `python-build-standalone`
  + `uv` (or falls back to system `python3`/`uv`); runs `uv sync` on
  demand against a bot's `pyproject.toml`; produces a `tokio::process::Command`
  ready to spawn the bot. Sync is idempotent via a `mtime:size` stamp at
  `<bot>/.akagi/synced.stamp`. `reset_sync_state()` wipes the stamp + venv
  for the user-triggered "Reinstall environment" path. See "Bundled
  runtime" below for how the bundled binaries are fetched and shipped.
- `sync_guard` — `SyncGuard`: per-bot mutual exclusion for `uv sync`.
  Both `BotManager::spawn_runner` (game-start sync) and the IPC
  `sync_bot_deps` command (Reinstall environment) acquire-or-bail on the
  bot name through a shared `Arc<Mutex<HashSet<String>>>` so two parallel
  syncs against the same venv can't trample each other.
- `runner` — `BotRunner` async trait + `SubprocessBot` impl. Talks JSONL
  over stdin/stdout, pumps stderr into `tracing` (`bot=<name>` field),
  enforces a 5 s default react timeout, and `kill_on_drop(true)` so a
  dropped runner can't leak children. `reset()` writes `[{"end_game"}]`,
  waits 500 ms, then SIGKILL's and respawns.
- `manager` — `BotManager`: subscribes to the `MjaiBus`, accumulates
  events between decision points (own tsumo / others' dahai-or-kakan /
  reach_accepted / hora / ryukyoku / end_kyoku / end_game), flushes the
  pending batch through the `BotRunner`, and broadcasts every
  `BotResponse` (including `MjaiEvent::None`) on the `BotResponseBus`.
  Spawn point is `start_game` carrying the bot's seat in the `id` field
  and the table's `num_players`. The manager picks `active_4p` or
  `active_3p` from `BotConfig` based on `num_players`; an empty slot for
  the matching mode means analysis-only for that game (no runner spawned).

## Adding a new bot

Drop a folder under `mjai_bot/`:

```
mjai_bot/<name>/
├── bot.py            # JSONL stdin → JSONL stdout
├── pyproject.toml    # uv-resolved deps; requires-python = ">=3.12"
├── manifest.toml     # OPTIONAL — settings schema (see below)
├── settings.toml     # OPTIONAL — current values; gitignored, written by Akagi
└── README.md         # bot-specific notes (model paths, license, etc.)
```

`bot.py` MUST:

- read one JSON array per line from stdin (a batch of `MjaiEvent`s)
- write one JSON object per line to stdout (a single mjai action — use
  `{"type":"none"}` when no action is owed)
- print **only** protocol JSON to stdout; logs go to stderr
- exit cleanly when it sees `{"type":"end_game"}` in a batch

The bot's seat is delivered three ways (pick whichever fits):

- **argv:** `python bot.py <player_id>` — matches mjai.app convention,
  so unmodified mjai.app bots run as-is.
- **env:** `AKAGI_PLAYER_ID=<player_id>`.
- **`start_game.id`:** the field on the first `start_game` event.

## Per-bot settings

A bot can ship a `manifest.toml` declaring its configurable knobs (API
URLs, keys, model selection, …). The frontend reads the manifest and
renders a generic settings form; the user's edits are persisted in
`settings.toml`.

Example `manifest.toml`:

```toml
manifest_version = 1

[bot]
name             = "mortal"
display          = "Mortal"
description      = "AGPL deep-RL mahjong bot."
version          = "0.5.0"
supported_modes  = ["4p"]   # ["3p"], ["4p"], or ["4p", "3p"]

[settings.api_url]
type    = "string"
label   = "API Server URL"
default = "https://api.example.com"
help    = "Endpoint for online inference. Leave blank to run offline."

[settings.api_key]
type    = "string"
label   = "API Key"
secret  = true
default = ""

[settings.online]
type    = "bool"
label   = "Online Mode"
default = false

[settings.temperature]
type    = "float"
label   = "Sampling Temperature"
default = 1.0
min     = 0.0
max     = 2.0
step    = 0.05

[settings.model]
type    = "enum"
label   = "Model"
default = "mortal.pth"
choices = ["mortal.pth", "mortal-1.5.pth"]
```

Field types: `string`, `bool`, `int`, `float`, `enum` (one of `choices`).
`secret = true` makes the frontend render a password input and Akagi
substitutes the value with `***` in tracing.

`supported_modes` declares which game modes this bot can play. Accepted
values: `"4p"` (yonma) and `"3p"` (sanma). Defaults to `["4p"]` when the
field is absent — pre-3p manifests stay 4p-only without edits. The Bots
route in the frontend disables the per-mode active toggle when the bot
doesn't support that mode.

Bots see the resolved settings (defaults ⊕ on-disk values, validated
against the manifest) via the env var `AKAGI_BOT_CONFIG`, which points
at a JSON file like:

```json
{
  "api_url": "https://api.example.com",
  "online": true
}
```

The bot script can `json.load(open(os.environ["AKAGI_BOT_CONFIG"]))`.
Bots that don't read the env var simply ignore it.

### Secrets caveat

For v1, `secret = true` only changes the *rendering* and *log
substitution* — the value is still stored in `settings.toml`. We
`.gitignore` `settings.toml` so it doesn't end up in source control.
OS keychain integration is on the roadmap; until then, treat
`settings.toml` as a credential file.

### When changes take effect

Updating settings does **not** restart the running bot subprocess. The
new values take effect on the next `start_game` event. The frontend
should warn the user accordingly.

## Installing from GitHub

The `install_bot_from_github` IPC command fetches the latest release of
a public GitHub repo, picks one asset, and drops it into
`mjai_bot/<name>/`. Frontend usage:

```ts
const info: BotInfo = await invoke('install_bot_from_github', {
  repo: 'Equim-chan/Mortal',          // owner/name
  assetGlob: 'mortal-v*.zip',         // optional; first .zip if omitted
  name: 'mortal',                     // optional; defaults to repo's second segment
});
```

Behaviour:

- Refuses to overwrite an existing `mjai_bot/<name>/` — the user must
  remove it first (or call `update_bot_from_manifest` for an explicit
  reinstall).
- Hits `https://api.github.com/repos/<repo>/releases/latest` anonymously.
  No token support in v1; only public repos.
- Asset selection: glob (rejecting zero or multiple matches) or first
  asset whose name ends in `.zip`.
- Streams the asset to a tempfile under `<bot_dir>/.downloads/`.
- Validates the zip header before extracting; rejects entries with `..`
  or absolute paths (zip-slip defence).
- If the archive has a single top-level directory (typical for release
  zips like `mortal-v0.5.0/…`), strips it.
- Validates that the extracted layout contains `bot.py` at the top.
- Atomic rename into `mjai_bot/<name>/`.
- After the rename, checks for the recommended `pyproject.toml` and
  `manifest.toml`. These are not required (only `bot.py` is), but if any
  are absent the install raises a `warn` toast — the target probably
  isn't the bot the user meant to install. The install still succeeds.
- Post-install: if a `PythonRuntime` is available and the extracted bot
  has a `pyproject.toml`, runs `uv sync` so dependency failures surface
  here instead of at game-start. On failure the bot dir stays in place
  so the user can retry via the per-bot Reinstall environment button
  without re-downloading. With no runtime available, sync is skipped
  with a `warn` toast and the install still succeeds.
- Progress is reported through `NotifyBus` with sticky id
  `bot-install-<name>` (info → info → info → success).

If the bot's `manifest.toml` declares a `[bot.source]` block, calling
`update_bot_from_manifest(name)` re-runs the install using the recorded
repo/glob. The previous `mjai_bot/<name>/` is removed first — settings
and other bot-local files are not preserved.

### Reinstall environment

The `sync_bot_deps(name, force)` IPC command re-runs `uv sync` for an
already-installed bot. With `force = true` (the only mode the frontend
button uses) the `.akagi/synced.stamp` and `.akagi/venv/` are wiped first
so a corrupted venv is rebuilt from scratch — incremental sync against a
broken venv can otherwise mask the breakage. Progress goes to `NotifyBus`
under sticky id `bot-sync-<name>`. The shared `SyncGuard` rejects a
second concurrent call for the same bot.

## Bundled runtime

End users get a packaged Akagi build that ships its own python and uv —
no system install required. The two binaries live under
`<resource_dir>/runtime/{python,uv}/<target-triple>/` and are picked up
by `PythonRuntime::locate(Some(resource_dir))` in `RuntimeMode::Bundled`.
On a dev box without bundled binaries, `locate` falls through to
`RuntimeMode::System` and uses whatever's on PATH.

### Layout

```
runtime/
├── python/<triple>/        # python-build-standalone tree (3.12.x)
│   ├── bin/python3         (linux/mac)
│   ├── lib/...
│   └── python.exe          (windows)
└── uv/<triple>/
    ├── uv                  (linux/mac)
    ├── uv.exe              (windows)
    └── uvx[.exe]
```

`<triple>` is the cargo target triple (e.g. `x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`).

### Fetching locally

```sh
scripts/fetch-runtime.sh                          # host triple
scripts/fetch-runtime.sh x86_64-pc-windows-msvc   # cross-target
scripts/fetch-runtime.sh --force                  # re-download
```

Override the versions via env vars: `PYTHON_VERSION`, `PBS_RELEASE`,
`UV_VERSION`. The script is idempotent — re-runs are no-ops once the
target binaries exist.

### Bundling

`tauri.conf.json` ships the tree via `bundle.resources = ["runtime/**/*"]`.
The `runtime/` directory is `.gitignore`'d (`runtime/*` with
`!runtime/.gitkeep` exception) so the placeholder file keeps the glob
non-empty even before `fetch-runtime.sh` runs. CI must re-run the
script once per matrix target before `tauri build`.

## Why subprocess

- AGPL bots (Mortal et al.) link via stdin/stdout pipe only — arms-length,
  not derivative work. See plan §10.
- Crash isolation: a bot SIGSEGV does not take Akagi down.
- Hot model swap = kill + spawn, no in-process state to flush.

In-process embedding (PyO3) was considered and rejected: linking
libpython makes single-binary distribution painful on Windows/macOS and
couples Akagi's lifecycle to libriichi's.

## Per-mode active bot (4p / 3p)

`BotConfig` stores two slots: `active_4p` and `active_3p`. `BotManager`
picks the slot matching `start_game.num_players` (3 → `active_3p`, else
`active_4p`). Empty slot ⇒ no runner spawned for that game (analysis
still runs).

Frontend → backend: `set_active_bot(mode, name)` IPC command, where
`mode` is `"4p"` or `"3p"` and `name` is the bot subdir name (or `""`
to clear the slot). The Bots route shows two switches per row.

`set_active_bot` refuses to activate a bot whose Python environment isn't
installed yet (`runtime::is_synced` is false — `pyproject.toml` present but
no up-to-date `.akagi/venv` + `synced.stamp`). Otherwise the bot's first
in-game spawn would run `uv sync`, which can exceed the react time limit and
error. `BotInfo.env_ready` surfaces this to the frontend, which disables the
activation switch (and shows a tooltip) until the env is installed. A bot with
no `pyproject.toml` has nothing to install and is always `env_ready`. The
install/sync IPC paths run `uv sync` to completion before returning, and the
frontend shows a non-dismissible blocking overlay for their whole duration, so
the user can't navigate away or start a game mid-install.

Pre-3p config files with a single `[bot] active = "..."` key are
migrated on load: the legacy value is moved to `active_4p` once via
`BotConfig::migrate_legacy_active`, and the on-disk file is rewritten
without the legacy field on the next persist.

## What's NOT here

- Mortal weights. Users place `mortal.pth` inside their `mjai_bot/mortal/`
  folder; Akagi never ships, fetches, or configures weight paths. The
  bot script loads them itself.
- HUD rendering. `BotResponse`s land on the broadcast bus; the HUD layer
  is a downstream consumer added later.
