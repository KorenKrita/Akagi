use crate::{
    bridge::{self, Bridge, Direction},
    config::Platform,
    event_bus::MjaiBus,
    inspector::InspectorWriter,
    logger::{BinaryLogger, Session},
    schema::{FrameDirection, FrameRaw, InspectorEntry},
};
use base64::Engine as _;
use chrono::Local;
use hudsucker::{
    futures::{Sink, SinkExt, Stream, StreamExt},
    hyper::{self, Request, Response, StatusCode, Uri},
    tokio_tungstenite::tungstenite::{self, Message},
    Body, HttpContext, HttpHandler, RequestOrResponse, WebSocketContext, WebSocketHandler,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

const TAG_CLIENT_TO_SERVER: u8 = 0;
const TAG_SERVER_TO_CLIENT: u8 = 1;

/// Shared, per-WS-upgrade bridge. Both directions of the same WebSocket
/// connection (client→server and server→client) need the same `Bridge`
/// instance because Majsoul's request/response correlation lives in the
/// parser's `pending` map: the Request travels client→server and the
/// matching Response travels server→client.
type SharedBridge = Arc<StdMutex<Box<dyn Bridge>>>;

/// Per-flow inspector identity. The map is keyed on the same
/// `SocketAddr` the bridges map uses, so they line up.
type FlowInspectorIds = Arc<StdMutex<HashMap<SocketAddr, String>>>;

#[derive(Clone)]
pub struct ProxyHandler {
    session: Arc<Session>,
    binary: Arc<BinaryLogger>,
    platform: Platform,
    bridges: Arc<StdMutex<HashMap<SocketAddr, SharedBridge>>>,
    next_flow_id: Arc<AtomicU64>,
    /// Optional fan-out for parsed mjai events. `None` keeps the proxy
    /// usable in tests and in standalone "log only" mode.
    mjai_tx: Option<MjaiBus>,
    /// Inspector writer. Cloned from `session.inspector()` at construction.
    /// Cheap to clone (Arc inside).
    inspector: InspectorWriter,
    /// Stable inspector flow id per client SocketAddr. Lets the inspector
    /// timeline group frames by flow even though the underlying bridge
    /// already lives at the SocketAddr key.
    inspector_flow_ids: FlowInspectorIds,
    /// Triggered by `stop_capture` to kick all in-flight WS flows. Without
    /// this, hudsucker's `with_graceful_shutdown` only blocks new
    /// connections; existing ones would drain naturally and the game
    /// client would never see a disconnect.
    force_close: Arc<Notify>,
}

impl ProxyHandler {
    pub fn new(
        session: Arc<Session>,
        platform: Platform,
        mjai_tx: Option<MjaiBus>,
        force_close: Arc<Notify>,
    ) -> anyhow::Result<Self> {
        let binary = session.binary_logger("proxy")?;
        let inspector = session.inspector();
        Ok(Self {
            session,
            binary,
            platform,
            bridges: Arc::new(StdMutex::new(HashMap::new())),
            next_flow_id: Arc::new(AtomicU64::new(1)),
            mjai_tx,
            inspector,
            inspector_flow_ids: Arc::new(StdMutex::new(HashMap::new())),
            force_close,
        })
    }

    /// Stable inspector flow id for `client`. Computed once on first
    /// frame from the per-platform subdir + the same `next_flow_id`
    /// counter the bridge map uses, so two flows from the same socket
    /// share the same id.
    fn inspector_flow_id(&self, client: SocketAddr) -> String {
        let mut map = self
            .inspector_flow_ids
            .lock()
            .expect("inspector_flow_ids mutex poisoned");
        map.entry(client)
            .or_insert_with(|| {
                let n = self.next_flow_id.load(Ordering::Relaxed).saturating_sub(1);
                format!("{}:{:06}", self.platform.subdir(), n)
            })
            .clone()
    }

    fn acquire_bridge(&self, client: SocketAddr, uri: &Uri) -> SharedBridge {
        let mut map = self.bridges.lock().expect("bridges mutex poisoned");
        map.entry(client)
            .or_insert_with(|| {
                let flow_id = self.next_flow_id.fetch_add(1, Ordering::Relaxed);
                let path = uri_path_slug(uri);
                let file_name = format!("{flow_id:06}-{path}.log");
                let label = format!("{} {} {}", self.platform.subdir(), client, uri);
                let flow_log =
                    match self
                        .session
                        .flow_logger(self.platform.subdir(), &file_name, label)
                    {
                        Ok(log) => Some(log),
                        Err(e) => {
                            warn!("failed to open flow log for {client}: {e:#}");
                            None
                        }
                    };
                Arc::new(StdMutex::new(bridge::for_platform(
                    self.platform,
                    flow_log,
                    Some(self.session.clone()),
                )))
            })
            .clone()
    }

    /// Drop our reference; if no other direction still holds the bridge,
    /// remove it from the map so per-connection state doesn't leak.
    fn release_bridge(&self, client: SocketAddr, bridge: SharedBridge) {
        drop(bridge);
        let mut map = self.bridges.lock().expect("bridges mutex poisoned");
        if let Some(existing) = map.get(&client) {
            // Only the map's own Arc remains → connection fully closed.
            if Arc::strong_count(existing) == 1 {
                map.remove(&client);
            }
        }
    }
}

impl HttpHandler for ProxyHandler {
    async fn handle_request(&mut self, ctx: &HttpContext, req: Request<Body>) -> RequestOrResponse {
        if req.uri().path() == "/ping" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("pong"))
                .expect("Failed to build ping response")
                .into();
        }
        // Pull Host header for diagnostics. The CDN routes by SNI/Host,
        // not by URI path, so when URIs come in as IP literals the Host
        // header is the only place the real hostname could survive — log
        // it next to the URI so we can tell at a glance whether a stuck
        // request had a real hostname or just an IP.
        let host_hdr = req
            .headers()
            .get(hyper::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        info!(
            target: "akagi::proxy::forward",
            "{} uri_host={} host_hdr={} {} v={:?} client={}",
            req.method(),
            req.uri().host().unwrap_or("-"),
            host_hdr,
            req.uri(),
            req.version(),
            ctx.client_addr,
        );
        req.into()
    }

    /// Skip MITM only for the narrow case of `CONNECT <ip-literal>:443`.
    ///
    /// Some game / app clients (catfood-studio Mahjong Soul Steam build)
    /// resolve DNS themselves and connect to a CDN by raw IP, but the CDN
    /// is multi-tenant and routes by SNI / Host. If we MITM, hudsucker's
    /// `normalize_request` strips the original Host header and hyper
    /// rebuilds it from the URI (= IP); rustls uses the URI host (= IP)
    /// as SNI. The CDN can't pick a tenant from that and serves the
    /// default 404 / 403. Raw-tunneling lets the client's own TLS / Host
    /// header reach the CDN untouched.
    ///
    /// We only do this on port 443 because that's where multi-tenant CDN
    /// fronts live. Game-specific gateways and APIs use alt ports (8443
    /// for the maj-soul WS gateway, 7201 for HTTPDNS, …), are
    /// single-tenant, and need MITM to be captured. Without the port
    /// narrowing we'd bypass the WS gateway too and lose all frame
    /// capture — observed in `test/logs/20260514-132645/`.
    ///
    /// Hostnames are always MITM'd; the maj-soul hostname endpoints
    /// (`mjusgs.mahjongsoul.com`, etc.) don't hit this code path.
    async fn should_intercept(&mut self, _ctx: &HttpContext, req: &Request<Body>) -> bool {
        let Some(host) = req.uri().host() else {
            return true;
        };
        if !is_ip_literal_host(host) {
            return true;
        }
        // IPv4 CONNECT URIs always carry an explicit port; default to 443
        // only as a defensive fallback.
        let port = req.uri().port_u16().unwrap_or(443);
        if port != 443 {
            return true;
        }
        info!(
            target: "akagi::proxy::forward",
            "raw-tunneling IP-literal CONNECT to {}",
            req.uri()
        );
        false
    }

    /// Diagnostic override of the default 502 path. The default just logs
    /// `"Failed to forward request: client error (Kind)"` with no URI and
    /// no inner cause. Walk the source chain so we can see whether a
    /// failed upstream forward was DNS, TCP, TLS handshake, ALPN, …
    /// Paired with the `handle_request` log above (which carries the
    /// host) — the two lines arrive back-to-back per failed flow.
    async fn handle_error(
        &mut self,
        ctx: &HttpContext,
        err: hudsucker::hyper_util::client::legacy::Error,
    ) -> Response<Body> {
        let mut chain = String::new();
        let mut next: Option<&dyn std::error::Error> = Some(&err);
        let mut depth = 0;
        while let Some(e) = next {
            if depth > 0 {
                chain.push_str(" ← ");
            }
            chain.push_str(&format!("{e}"));
            next = e.source();
            depth += 1;
            // Guard against pathological loops.
            if depth > 16 {
                chain.push_str(" ← [chain truncated]");
                break;
            }
        }
        error!(
            target: "akagi::proxy::forward",
            "upstream forward failed: client={} chain=[{chain}]",
            ctx.client_addr,
        );
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::empty())
            .expect("Failed to build 502")
    }
}

impl WebSocketHandler for ProxyHandler {
    async fn handle_websocket(
        mut self,
        ctx: WebSocketContext,
        mut stream: impl Stream<Item = Result<Message, tungstenite::Error>> + Unpin + Send + 'static,
        mut sink: impl Sink<Message, Error = tungstenite::Error> + Unpin + Send + 'static,
    ) {
        let client = client_addr(&ctx);
        let server_uri = server_uri(&ctx);
        let bridge = self.acquire_bridge(client, &server_uri);
        let force_close = self.force_close.clone();

        loop {
            tokio::select! {
                biased;
                _ = force_close.notified() => {
                    info!("force-closing WS flow for {client}");
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                next = stream.next() => {
                    let Some(message) = next else { break };
                    match message {
                        Ok(message) => {
                            let Some(out) = self.handle_message(&ctx, message, &bridge).await else {
                                continue;
                            };
                            match sink.send(out).await {
                                Ok(()) => (),
                                // Peer already gone — normal at end of game / lobby.
                                Err(tungstenite::Error::ConnectionClosed)
                                | Err(tungstenite::Error::AlreadyClosed) => break,
                                Err(e) => {
                                    error!("WebSocket send error: {e}");
                                    break;
                                }
                            }
                        }
                        Err(tungstenite::Error::ConnectionClosed)
                        | Err(tungstenite::Error::AlreadyClosed) => break,
                        Err(e) => {
                            error!("WebSocket recv error: {e}");
                            match sink.send(Message::Close(None)).await {
                                Ok(())
                                | Err(tungstenite::Error::ConnectionClosed)
                                | Err(tungstenite::Error::AlreadyClosed) => (),
                                Err(e) => error!("WebSocket close error: {e}"),
                            }
                            break;
                        }
                    }
                }
            }
        }

        self.release_bridge(client, bridge);
    }
}

impl ProxyHandler {
    async fn handle_message(
        &mut self,
        ctx: &WebSocketContext,
        msg: Message,
        bridge: &SharedBridge,
    ) -> Option<Message> {
        let client = client_addr(ctx);
        let (tag, dir, dir_arrow, uri) = match ctx {
            WebSocketContext::ServerToClient { src, .. } => (
                TAG_SERVER_TO_CLIENT,
                Direction::Down,
                '\u{2193}',
                src.to_string(),
            ),
            WebSocketContext::ClientToServer { dst, .. } => (
                TAG_CLIENT_TO_SERVER,
                Direction::Up,
                '\u{2191}',
                dst.to_string(),
            ),
        };

        match &msg {
            Message::Binary(buf) => {
                debug!("{dir_arrow} {uri} binary len={}", buf.len());
                self.binary.write(tag, buf);
                let result = {
                    let mut b = bridge.lock().expect("bridge mutex poisoned");
                    b.parse(dir, buf)
                };
                self.record_frame(client, dir, FrameRaw::Binary(b64(buf)), buf.len(), &result);
                self.dispatch_events(dir_arrow, &uri, result.events);
            }
            Message::Text(t) => {
                debug!("{dir_arrow} {uri} text len={}", t.len());
                let buf = t.as_bytes();
                self.binary.write(tag, buf);
                let result = {
                    let mut b = bridge.lock().expect("bridge mutex poisoned");
                    b.parse(dir, buf)
                };
                self.record_frame(
                    client,
                    dir,
                    FrameRaw::Text(t.to_string()),
                    buf.len(),
                    &result,
                );
                self.dispatch_events(dir_arrow, &uri, result.events);
            }
            Message::Close(_) => debug!("{dir_arrow} {uri} close"),
            _ => {}
        }

        if let Message::Frame(_) = &msg {
            warn!("unexpected raw frame at {uri}");
        }

        Some(msg)
    }

    fn dispatch_events(&self, dir_arrow: char, uri: &str, events: Vec<crate::schema::MjaiEvent>) {
        if events.is_empty() {
            return;
        }
        debug!("{dir_arrow} {uri} bridge emitted {} event(s)", events.len());
        if let Some(tx) = &self.mjai_tx {
            for ev in events {
                // No subscribers is fine — broadcast just drops.
                let _ = tx.send(ev);
            }
        }
    }

    fn record_frame(
        &self,
        client: SocketAddr,
        dir: Direction,
        raw: FrameRaw,
        size: usize,
        result: &bridge::ParseResult,
    ) {
        let direction = match dir {
            Direction::Down => FrameDirection::Down,
            Direction::Up => FrameDirection::Up,
        };
        self.inspector.record(InspectorEntry::WsFrame {
            ts_ms: Local::now().timestamp_millis(),
            direction,
            flow_id: self.inspector_flow_id(client),
            size,
            raw,
            parsed: result.parsed.clone(),
            emitted: result.events.len(),
        });
    }
}

fn b64(buf: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(buf)
}

fn client_addr(ctx: &WebSocketContext) -> SocketAddr {
    match ctx {
        WebSocketContext::ClientToServer { src, .. } => *src,
        WebSocketContext::ServerToClient { dst, .. } => *dst,
    }
}

fn server_uri(ctx: &WebSocketContext) -> Uri {
    match ctx {
        WebSocketContext::ClientToServer { dst, .. } => dst.clone(),
        WebSocketContext::ServerToClient { src, .. } => src.clone(),
    }
}

/// Sanitize the URI path into a filename-safe slug. `/game-gateway` →
/// `game-gateway`, `/` → `root`, anything outside `[A-Za-z0-9_-]` becomes
/// `_`.
fn uri_path_slug(uri: &Uri) -> String {
    let raw = uri.path().trim_matches('/');
    if raw.is_empty() {
        return "root".into();
    }
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `true` when `host` (as returned by `Uri::host`) is a literal IPv4 or
/// IPv6 address. IPv6 hosts come back from `Uri::host` wrapped in `[…]`
/// per RFC 3986; `IpAddr::from_str` rejects the brackets so we strip them
/// before parsing.
fn is_ip_literal_host(host: &str) -> bool {
    let stripped = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    stripped.parse::<std::net::IpAddr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::is_ip_literal_host;

    #[test]
    fn ipv4_literal_is_detected() {
        assert!(is_ip_literal_host("156.238.128.60"));
        assert!(is_ip_literal_host("127.0.0.1"));
        assert!(is_ip_literal_host("0.0.0.0"));
    }

    #[test]
    fn ipv6_literal_with_brackets_is_detected() {
        // `Uri::host` returns IPv6 wrapped in brackets.
        assert!(is_ip_literal_host("[::1]"));
        assert!(is_ip_literal_host("[2001:db8::1]"));
        assert!(is_ip_literal_host("[fe80::1234:5678:9abc:def0]"));
    }

    #[test]
    fn hostnames_are_not_ip_literals() {
        // The hosts we explicitly do want MITM'd.
        assert!(!is_ip_literal_host("game.maj-soul.com"));
        assert!(!is_ip_literal_host("mjusgs.mahjongsoul.com"));
        assert!(!is_ip_literal_host("tenhou.net"));
        assert!(!is_ip_literal_host("localhost"));
        // Numeric-leading hostname (real-world: `3839.com` style).
        assert!(!is_ip_literal_host("4399.cn"));
    }

    #[test]
    fn edge_cases_do_not_panic() {
        // Empty / malformed inputs return false rather than panicking.
        assert!(!is_ip_literal_host(""));
        assert!(!is_ip_literal_host("["));
        assert!(!is_ip_literal_host("[]"));
        assert!(!is_ip_literal_host("[not-an-ip]"));
        // Trailing dot on a hostname.
        assert!(!is_ip_literal_host("example.com."));
    }
}
