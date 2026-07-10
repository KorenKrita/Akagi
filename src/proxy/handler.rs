use crate::{
    bridge::{self, Bridge, Direction},
    config::Platform,
    event_bus::{MjaiBus, NotifyBus},
    inspector::InspectorWriter,
    logger::{BinaryLogger, Session},
    schema::{FrameDirection, FrameRaw, InspectorEntry, Notification},
};
use base64::Engine as _;
use chrono::Local;
use hudsucker::{
    futures::{Sink, SinkExt, Stream, StreamExt},
    hyper::{self, Method, Request, Response, StatusCode, Uri},
    tokio_tungstenite::tungstenite::{self, Message},
    Body, HttpContext, HttpHandler, RequestOrResponse, WebSocketContext, WebSocketHandler,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    /// Optional fan-out for user-facing toasts. `None` in "log only" mode.
    notify_tx: Option<NotifyBus>,
    /// Latches on the first refused loopback CONNECT. A misconfigured
    /// redirector produces one per socket the game opens, and the user only
    /// needs telling once — the log keeps every occurrence.
    loopback_notified: Arc<AtomicBool>,
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
        notify_tx: Option<NotifyBus>,
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
            notify_tx,
            loopback_notified: Arc::new(AtomicBool::new(false)),
            inspector,
            inspector_flow_ids: Arc::new(StdMutex::new(HashMap::new())),
            force_close,
        })
    }

    /// Tell the user, once, that their redirector is proxying loopback. The
    /// log line alone is not enough — nobody opens the log while staring at a
    /// game that never finishes loading.
    ///
    /// Returns `true` if this was the first refusal (caller logs at `WARN`;
    /// later ones are `DEBUG` so a chatty client can't flood the file).
    fn notify_loopback_refused(&self, authority: &Uri) -> bool {
        if self.loopback_notified.swap(true, Ordering::Relaxed) {
            return false;
        }
        if let Some(tx) = &self.notify_tx {
            // Sticky: this blocks play until the user fixes the redirector.
            let _ = tx.send(
                Notification::warn("Proxy redirector is sending loopback traffic to Akagi")
                    .body(format!(
                        "Refused CONNECT to {authority}. Games talk to themselves over \
                         loopback, and proxying that hangs them on the loading screen. \
                         Exclude localhost / 127.0.0.1 / ::1 from the rule that redirects \
                         the game — in Proxifier, enable the \"Localhost\" rule (action: \
                         Direct) and move it above the game rule.",
                    ))
                    .sticky()
                    .id("proxy-loopback-connect"),
            );
        }
        true
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

        // A game client never legitimately CONNECTs to loopback — the proxy
        // itself lives there. Seeing one means the user's redirector
        // (Proxifier &c.) matched the game for *any* target host and swept up
        // the game's own internal `socketpair()` self-connect.
        //
        // Refusing here rather than tunneling it is load-bearing. hudsucker's
        // `process_connect` answers 200 and then blocks reading the client's
        // first 4 bytes *before* it opens the upstream socket; a self-connect's
        // client side never speaks first (it is waiting on its own `accept()`),
        // so the tunnel deadlocks and the game wedges on its loading screen.
        // Returning a `Response` short-circuits hudsucker's `proxy()` before
        // that blocking read is ever reached.
        //
        // Tunneling it correctly is not an option either: libcurl's
        // `socketpair()` emulation checks that the address it accepted matches
        // its connecting socket's local address, which a proxied hop breaks.
        if is_loopback_connect(req.method(), req.uri()) {
            if self.notify_loopback_refused(req.uri()) {
                warn!(
                    target: "akagi::proxy::forward",
                    "refusing CONNECT to loopback {} from {}: your proxy redirector is sending the \
                     game's own internal loopback sockets through Akagi, which would hang the game. \
                     Exclude localhost / 127.0.0.1 / ::1 from the rule that redirects the game (in \
                     Proxifier: enable the \"Localhost\" Direct rule and move it above the game rules).",
                    req.uri(),
                    ctx.client_addr,
                );
            } else {
                debug!(
                    target: "akagi::proxy::forward",
                    "refusing CONNECT to loopback {} from {}",
                    req.uri(),
                    ctx.client_addr,
                );
            }
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::empty())
                .expect("Failed to build loopback CONNECT refusal")
                .into();
        }

        req.into()
    }

    /// Skip MITM only for the narrow case of `CONNECT <ip-literal>:443` on
    /// **Mahjong Soul** (see [`should_raw_tunnel`]).
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
    /// narrowing we'd bypass the WS gateway too and lose all frame capture.
    ///
    /// The bypass is **gated to Mahjong Soul**: Riichi City reaches its
    /// single-tenant game server by raw IP on 443 and presents a normal
    /// (MITM-able) certificate, so raw-tunneling there would silently drop
    /// the gameplay WebSocket — the only `<ip>:443` CONNECT (the WSS gameplay
    /// flow) would be bypassed and no frames would reach the bridge.
    ///
    /// Hostnames are always MITM'd; the maj-soul hostname endpoints
    /// (`mjusgs.mahjongsoul.com`, etc.) don't hit this code path.
    async fn should_intercept(&mut self, _ctx: &HttpContext, req: &Request<Body>) -> bool {
        let Some(host) = req.uri().host() else {
            return true;
        };
        // IPv4 CONNECT URIs always carry an explicit port; default to 443
        // only as a defensive fallback.
        let port = req.uri().port_u16().unwrap_or(443);
        if should_raw_tunnel(self.platform, host, port) {
            info!(
                target: "akagi::proxy::forward",
                "raw-tunneling IP-literal CONNECT to {}",
                req.uri()
            );
            return false;
        }
        true
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

/// Whether a `CONNECT host:port` should bypass MITM with a raw TCP tunnel.
///
/// True only for the Mahjong Soul multi-tenant CDN case — an IP-literal host
/// on port 443 — where MITM would set SNI = IP and the CDN would serve its
/// default error page. Every other platform (notably Riichi City, whose
/// single-tenant game server is reached by raw IP on 443 and presents a
/// MITM-able cert) is intercepted so its gameplay WebSocket is captured.
fn should_raw_tunnel(platform: Platform, host: &str, port: u16) -> bool {
    platform == Platform::Majsoul && port == 443 && is_ip_literal_host(host)
}

/// IPv6 hosts come back from `Uri::host` wrapped in `[…]` per RFC 3986;
/// `IpAddr::from_str` rejects the brackets, so strip them before parsing.
fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
}

/// `true` when `host` (as returned by `Uri::host`) is a literal IPv4 or
/// IPv6 address.
fn is_ip_literal_host(host: &str) -> bool {
    strip_brackets(host).parse::<std::net::IpAddr>().is_ok()
}

/// `true` when `host` names the loopback interface: any address in
/// `127.0.0.0/8`, `::1`, or the reserved name `localhost`.
fn is_loopback_host(host: &str) -> bool {
    let host = strip_brackets(host);
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// `true` for a `CONNECT` whose authority names the loopback interface — the
/// signature of a redirector that is proxying the game's own internal sockets.
/// See the refusal in [`ProxyHandler::handle_request`] for why these must not
/// be tunneled.
fn is_loopback_connect(method: &Method, uri: &Uri) -> bool {
    method == Method::CONNECT && uri.host().is_some_and(is_loopback_host)
}

#[cfg(test)]
mod tests {
    use super::{is_ip_literal_host, is_loopback_connect, is_loopback_host, should_raw_tunnel};
    use crate::config::Platform;
    use hudsucker::hyper::{Method, Uri};

    /// Regression: a redirector matching the game for *any* target host sweeps
    /// up its internal `socketpair()` self-connect. Tunneling that deadlocks —
    /// hudsucker blocks reading the client's first bytes before it dials
    /// upstream, and a self-connect's client side never speaks first — which
    /// hung the Mahjong Soul Steam client on its loading screen forever.
    #[test]
    fn loopback_connect_is_refused() {
        let connect = Method::CONNECT;
        // The exact shape seen in the wild: game's own ephemeral listener.
        assert!(is_loopback_connect(
            &connect,
            &"127.0.0.1:65423".parse::<Uri>().unwrap()
        ));
        // The whole 127.0.0.0/8 block, IPv6 loopback, and the reserved name.
        assert!(is_loopback_connect(
            &connect,
            &"127.9.9.9:1234".parse::<Uri>().unwrap()
        ));
        assert!(is_loopback_connect(
            &connect,
            &"[::1]:8080".parse::<Uri>().unwrap()
        ));
        assert!(is_loopback_connect(
            &connect,
            &"localhost:23410".parse::<Uri>().unwrap()
        ));
    }

    /// The refusal must not touch real game traffic, nor non-`CONNECT` methods
    /// (Akagi's own `GET /ping` health probe arrives on loopback).
    #[test]
    fn non_loopback_connect_and_other_methods_pass_through() {
        assert!(!is_loopback_connect(
            &Method::CONNECT,
            &"156.238.128.63:443".parse::<Uri>().unwrap()
        ));
        assert!(!is_loopback_connect(
            &Method::CONNECT,
            &"game.maj-soul.com:443".parse::<Uri>().unwrap()
        ));
        assert!(!is_loopback_connect(
            &Method::GET,
            &"http://127.0.0.1:23410/ping".parse::<Uri>().unwrap()
        ));
        // No authority at all (origin-form request URI).
        assert!(!is_loopback_connect(
            &Method::GET,
            &"/ping".parse::<Uri>().unwrap()
        ));
    }

    #[test]
    fn loopback_hosts_are_detected() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.255.255.254"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LocalHost"));
    }

    #[test]
    fn non_loopback_hosts_are_not_detected() {
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("156.238.128.63"));
        assert!(!is_loopback_host("[2001:db8::1]"));
        assert!(!is_loopback_host("game.maj-soul.com"));
        // Hostname that merely starts with the loopback label.
        assert!(!is_loopback_host("localhost.example.com"));
        assert!(!is_loopback_host(""));
    }

    /// Mahjong Soul Steam build: IP-literal CONNECT on 443 is raw-tunneled so
    /// the multi-tenant CDN keeps the client's own SNI.
    #[test]
    fn majsoul_ip_literal_443_is_raw_tunneled() {
        assert!(should_raw_tunnel(Platform::Majsoul, "13.112.183.79", 443));
        assert!(should_raw_tunnel(Platform::Majsoul, "[2001:db8::1]", 443));
    }

    /// Regression: Riichi City's gameplay WSS goes to its single-tenant game
    /// server by raw IP on 443. It must be MITM'd — raw-tunneling silently
    /// dropped every gameplay frame in a real capture.
    #[test]
    fn riichi_city_ip_literal_443_is_intercepted() {
        assert!(!should_raw_tunnel(
            Platform::RiichiCity,
            "13.112.183.79",
            443
        ));
    }

    /// The bypass never applies to hostnames or non-443 ports, even on Majsoul.
    #[test]
    fn raw_tunnel_only_for_ip_literal_443() {
        assert!(!should_raw_tunnel(
            Platform::Majsoul,
            "mjusgs.mahjongsoul.com",
            443
        ));
        assert!(!should_raw_tunnel(Platform::Majsoul, "13.112.183.79", 80));
        assert!(!should_raw_tunnel(Platform::Majsoul, "13.112.183.79", 8443));
        assert!(!should_raw_tunnel(Platform::Tenhou, "13.112.183.79", 443));
    }

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
