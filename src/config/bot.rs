use serde::{Deserialize, Serialize};

/// Optional cloud-inference settings for the built-in (native) bot.
///
/// When [`NativeApiConfig::is_active`] is true, the built-in bot proxies each
/// decision to a remote inference server (`POST /v3/react`) instead of running
/// the embedded local model. The local model stays loaded as a fallback: if the
/// server is unreachable, rate-limited, or the key is invalid, the bot silently
/// plays the local model's move so a live game never stalls.
///
/// Read fresh at every decision by `crate::bot::native::NativeBot`, so toggling
/// the API, correcting the key, or switching models takes effect on the next
/// move of the game in progress.
///
/// Everything defaults empty / disabled so a fresh install uses the fully
/// offline local model until the user opts in and pastes a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeApiConfig {
    /// Route built-in-bot decisions through the remote API. Ignored unless a
    /// `base_url` and `key` are also set (see [`NativeApiConfig::is_active`]).
    pub enabled: bool,
    /// Base URL of the inference server, e.g. `https://host` or
    /// `http://127.0.0.1:8080`. A trailing slash is tolerated.
    pub base_url: String,
    /// Bearer API key (32 alphanumeric chars). Obtain one by redeeming a code.
    pub key: String,
    /// Model id for 4-player games (from `GET /v3/models`). Empty ⇒ let the
    /// server pick its default 4p model.
    pub model_4p: String,
    /// Model id for 3-player games. Empty ⇒ server default 3p model.
    pub model_3p: String,
}

/// Default inference server. Pre-filled so users don't have to type it; the API
/// still stays inactive until they enable it and paste a key.
pub const DEFAULT_API_BASE_URL: &str = "https://mjapi.shinkuan.me";

impl Default for NativeApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: DEFAULT_API_BASE_URL.to_string(),
            key: String::new(),
            model_4p: String::new(),
            model_3p: String::new(),
        }
    }
}

impl NativeApiConfig {
    /// True only when the API path is fully configured (opted in with both a
    /// server URL and a key). The manager uses this to decide whether to build
    /// the API-backed runner or the local one.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.base_url.trim().is_empty() && !self.key.trim().is_empty()
    }

    /// Model id to request for the given player count. Empty string ⇒ omit the
    /// `model` field and let the server pick its game default.
    pub fn model_for(&self, num_players: u8) -> &str {
        if num_players == 3 {
            &self.model_3p
        } else {
            &self.model_4p
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BotConfig {
    /// Master switch. When `false`, no `BotManager` is spawned and the
    /// MJAI event bus runs without a consumer.
    pub enabled: bool,
    /// Active bot for 4-player (yonma) games. Subdirectory of `dir`.
    ///
    /// Reads the legacy `active` key on first load (see `migrate_legacy_active`)
    /// so existing config files keep working.
    pub active_4p: String,
    /// Active bot for 3-player (sanma) games. Empty string ⇒ no bot
    /// configured for 3p (analysis-only mode in 3p matches).
    pub active_3p: String,
    /// Legacy field, kept for one release for migration purposes. New code
    /// reads `active_4p` / `active_3p`. If the on-disk config has only
    /// `active` set, `active_4p` is populated from it during deserialise.
    #[serde(skip_serializing)]
    pub active: String,
    /// Run `uv sync` automatically before spawning the bot. Disabling
    /// makes startup faster on slow disks but assumes the venv is
    /// already in sync — usually for advanced users.
    pub auto_sync: bool,
    /// Root directory containing one subdir per bot. Resolved with the
    /// same fallback chain as other directory configs (`util::resolve_dir`).
    pub dir: String,
    /// Optional cloud-inference settings for the built-in native bot. When
    /// active, the native bot proxies decisions to a remote server instead of
    /// running the embedded model. See [`NativeApiConfig`].
    pub api: NativeApiConfig,
}

impl BotConfig {
    /// Pick the active bot name for the given player count. 3 ⇒ `active_3p`,
    /// anything else ⇒ `active_4p`.
    pub fn active_for(&self, num_players: u8) -> &str {
        if num_players == 3 {
            &self.active_3p
        } else {
            &self.active_4p
        }
    }

    /// Migrate the legacy `active` field into `active_4p` if the user's
    /// config file predates the per-mode split.
    pub fn migrate_legacy_active(&mut self) {
        if !self.active.is_empty() && self.active_4p.is_empty() {
            self.active_4p = std::mem::take(&mut self.active);
        } else {
            // Drop any legacy value we read; future writes won't include it.
            self.active.clear();
        }
    }
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            // Enabled by default: the built-in native bots (below) are always
            // available and need no install, so a fresh install shows bot
            // recommendations out of the box.
            enabled: true,
            // Built-in, pure-Rust default bots (no Python / libriichi). See
            // `crate::bot::native`. They are always available and need no
            // install, so they make sensible out-of-the-box defaults.
            active_4p: crate::bot::native::NATIVE_4P.to_string(),
            active_3p: crate::bot::native::NATIVE_3P.to_string(),
            active: String::new(),
            auto_sync: true,
            dir: "mjai_bot".to_string(),
            api: NativeApiConfig::default(),
        }
    }
}
