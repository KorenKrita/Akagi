# Proxy Module

MITM HTTP/HTTPS/WebSocket proxy built on [hudsucker](https://crates.io/crates/hudsucker). Used to intercept game traffic (e.g. Majsoul WebSocket frames) for protocol parsing and AI integration.

The hudsucker dependency in `Cargo.toml` opts out of default features and re-enables `rcgen-ca`, `rustls-client`, and `http2`. The `http2` flag is load-bearing: without it, both the MITM TLS server (`rcgen_authority`) and the upstream hyper-rustls connector skip `h2` ALPN / `enable_http2()`, which breaks HTTP/2-only origin servers — the Steam build of Mahjong Soul hits exactly this and stalls at "獲取[配置初始化文件]失敗 [3010]".

## Files

- `mod.rs` — Public entry: `start_proxy(config, session, shutdown)`. Builds the proxy from `ProxyConfig` and shares the logging `Session`.
- `ca.rs` — CA certificate management. Loads `akagi-ca.cer` + `akagi-ca.key` from `ca_dir`, generating a fresh self-signed CA on first run. Also writes the cert in `.crt` / `.pem` / `.der` form and the key in `.key.der` form for OS / tooling compatibility.
- `handler.rs` — `ProxyHandler` implementing `HttpHandler` + `WebSocketHandler`. Logs WS frame direction/length to text log and writes raw binary frames to `<session>/proxy.binlog`. Extend here to parse protocol messages. The HTTP side logs every forwarded request and any upstream-forward failure with full error source chain under tracing target `akagi::proxy::forward` — filter on that target when diagnosing "stuck on loading" reports. By default, `should_intercept` raw-tunnels any CONNECT whose authority is an IP literal so app-side SNI/Host headers reach multi-tenant CDNs intact; hostnames continue to be MITM'd. Setting `proxy.force_mitm_all = true` disables that compatibility bypass.
- `upstream.rs` — Custom hyper-rustls connector used for the proxy → server leg. Skips server-cert validation (`NoVerify`) on purpose so we can talk to CDN IPs whose default cert covers only DNS hostnames, matching the game client's own loose validation. Replaces what hudsucker's `with_rustls_connector` would have given us. See the module-level doc for the threat-model justification.

## CA Certificate

On first run, a self-signed root CA is generated at `<ca_dir>/akagi-ca.{cer,crt,pem,der}` (default `./ca`), with the matching private key written as `akagi-ca.key` (PEM) and `akagi-ca.key.der` (DER). To intercept TLS traffic the user must trust the CA cert in their OS / browser store — pick whichever extension that store accepts (Windows commonly wants `.cer`/`.crt`/`.der`, Linux/Firefox `.pem`/`.crt`). Subsequent runs reuse the existing CA and back-fill any missing format files.

### `ca_dir` resolution

If `ca_dir` is absolute, it's used as-is. If relative (default `./ca`), resolution mirrors config loading:

1. `<exe_dir>/<ca_dir>` if it exists
2. `<cwd>/<ca_dir>` if it exists
3. Otherwise create at `<exe_dir>/<ca_dir>` (preferred), falling back to `<cwd>/<ca_dir>` if exe path is unavailable

The proxy responds to `GET /ping` with `pong` — useful for liveness checks.

## Configuration

Lives under `[proxy]` in `config.toml`:

```toml
[proxy]
enabled = true
addr = "127.0.0.1:23410"
ca_dir = "./ca"
```

## Adding traffic interception

Edit `handler.rs::ProxyHandler::handle_message`. The `WebSocketContext` distinguishes upstream (`ClientToServer`) vs downstream (`ServerToClient`) frames. Return `Some(msg)` to forward unchanged, return a modified `Message` to inject changes, or return `None` to drop.

For protobuf parsing, see `src/bridge/majsoul/parser.rs` — Majsoul WS frames use a 5-layer format: `[type byte][BaseMessage protobuf][inner message][XOR-encrypted action]`.

## Adding state to the handler

Currently `ProxyHandler` is unit-struct + `Clone`. To add shared state (sender channel, parser, settings), give it an `Arc<...>` field and clone is cheap. See MajsoulMax `handler.rs` for the pattern.
