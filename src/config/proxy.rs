use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub addr: String,
    pub ca_dir: PathBuf,
    /// Explicit switch for routing proxy-to-server traffic through
    /// `upstream`. Keeping the address separate lets users save a proxy
    /// endpoint without using it for every session.
    pub upstream_enabled: bool,
    /// Optional upstream HTTP proxy. When set, all proxy-to-server traffic
    /// goes through this proxy with HTTP CONNECT.
    pub upstream: Option<String>,
    /// Force every CONNECT flow through MITM instead of raw-tunneling
    /// IP-literal CDN connections. This can break clients that rely on
    /// custom TLS/SNI behavior, so the default stays compatibility-first.
    pub force_mitm_all: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            addr: "127.0.0.1:23410".to_string(),
            ca_dir: PathBuf::from("./ca"),
            upstream_enabled: false,
            upstream: None,
            force_mitm_all: false,
        }
    }
}
