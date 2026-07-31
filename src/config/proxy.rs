use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub addr: String,
    pub ca_dir: PathBuf,
    /// Correct the certificate report a Mahjong Soul standalone client
    /// sends about its gateway connections, substituting the certificates
    /// Akagi observed upstream for the ones it served itself.
    ///
    /// On by default because leaving it off means the client reports
    /// Akagi's CA by name on every login. Turn it off to capture what the
    /// client *would* have said — which is the only way to check the
    /// correction is still complete after a client update.
    ///
    /// MITM-mode only: the chromium backend has nothing to rewrite, and a
    /// browser cannot report peer certificates in the first place.
    /// See `src/proxy/rewrite/majsoul_cert.rs`.
    pub rewrite_certificate_report: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            addr: "127.0.0.1:23410".to_string(),
            ca_dir: PathBuf::from("./ca"),
            rewrite_certificate_report: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The correction is on by default: with it off, a standalone client
    /// names Akagi's CA to the operator on every single login.
    #[test]
    fn certificate_report_is_corrected_by_default() {
        assert!(ProxyConfig::default().rewrite_certificate_report);
    }

    /// A `config.toml` written before the field existed must still load,
    /// and must pick up the default rather than silently disabling it.
    #[test]
    fn older_configs_gain_the_correction() {
        let cfg: ProxyConfig =
            toml::from_str("enabled = true\naddr = \"127.0.0.1:23410\"\nca_dir = \"./ca\"")
                .expect("older config must still parse");
        assert!(cfg.rewrite_certificate_report);
    }
}
