mod ca;
mod handler;
mod upstream;

pub use handler::ProxyHandler;

use crate::{
    config::{Platform, ProxyConfig},
    event_bus::MjaiBus,
    logger::Session,
    util::resolve_dir,
};
use anyhow::{Context, Result};
use hudsucker::Proxy;
use std::{future::Future, net::SocketAddr, str::FromStr, sync::Arc};
use tokio::sync::Notify;
use tracing::info;

pub async fn start_proxy<F>(
    config: ProxyConfig,
    platform: Platform,
    session: Arc<Session>,
    mjai_tx: Option<MjaiBus>,
    force_close: Arc<Notify>,
    shutdown: F,
) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let ca_dir = resolve_dir(&config.ca_dir);
    info!("Using CA dir: {}", ca_dir.display());

    let ca = ca::load_or_generate(&ca_dir)?;
    let addr = SocketAddr::from_str(&config.addr)
        .with_context(|| format!("Invalid proxy addr: {}", config.addr))?;

    let handler = ProxyHandler::new(session.clone(), platform, mjai_tx, force_close)?;

    info!("Starting proxy on {addr}");

    let proxy = Proxy::builder()
        .with_addr(addr)
        .with_ca(ca)
        .with_http_connector(upstream::http_connector())
        .with_http_handler(handler.clone())
        .with_websocket_handler(handler)
        .with_websocket_connector(upstream::websocket_connector())
        .with_graceful_shutdown(shutdown)
        .build()
        .context("Failed to build proxy")?;

    proxy.start().await.context("Proxy stopped with error")
}
