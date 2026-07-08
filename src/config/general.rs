use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Set to `true` once the first-run setup wizard has finished.
    /// Existing pre-wizard configs default to `true` via migration so
    /// upgraded users don't see the wizard.
    pub first_run_completed: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            first_run_completed: false,
        }
    }
}
