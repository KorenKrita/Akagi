use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Set to `true` once the first-run setup wizard has finished.
    /// Existing pre-wizard configs default to `true` via migration so
    /// upgraded users don't see the wizard.
    pub first_run_completed: bool,
    /// Unlocks developer-only settings in the UI — currently the
    /// cloud-inference server URL (`bot.api.base_url`), which regular users
    /// have no reason to change and scammers have every reason to ask them
    /// to. Off by default; the flag only gates the frontend editors, the
    /// backend honours whatever the config file says.
    pub developer_mode: bool,
}
