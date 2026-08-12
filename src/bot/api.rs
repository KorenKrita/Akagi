//! HTTP client for the remote inference API (`/v3/*`), as published by the
//! inference server's own API documentation.
//!
//! The service is **stateless**: every [`ApiClient::react`] call uploads the
//! current kyoku's mjai event stream (from the bot's seat perspective, ending
//! at the decision point) and gets back the move to play. This module is a thin
//! typed wrapper around those endpoints — the mjai stream shaping / censoring
//! lives in [`crate::bot::native`], the caller.
//!
//! Two consumers:
//! - the built-in bot ([`crate::bot::native::NativeBot`]) calls
//!   [`ApiClient::react`] at each decision point, while cloud inference is on;
//! - the IPC layer ([`crate::ipc::commands`]) calls [`redeem`],
//!   [`ApiClient::key_status`], [`ApiClient::models`] and [`health`] so the
//!   frontend can redeem codes and inspect a key.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Timeout for the management endpoints (key status, models, redeem, health).
/// These run from the UI, never on a game's critical path, so they can afford
/// to wait out a slow server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Timeout for [`ApiClient::react`], which sits on a live game's critical path.
/// Majsoul's turn timer is ~5s plus a shared time bank, and a reach costs two
/// react calls (declare, then discard), so the whole two-step has to fit inside
/// one turn. Keep this tight: a hung server falls back to the local model
/// promptly instead of making the bot miss its turn. Repeated failures are then
/// suppressed by the caller's circuit breaker, so only the first turn of an
/// outage pays this cost.
pub const REACT_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Response from `POST /v3/react`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReactResponse {
    /// The move to play, as a standard mjai event (`actor == player_id`).
    /// `None` when the seat has no legal action for the final event.
    #[serde(default)]
    pub reaction: Option<Value>,
    /// Up to `topk` coarse action labels ranked by probability. `candidates[0]`
    /// corresponds to `reaction`.
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// The model id that actually served the request.
    #[serde(default)]
    pub model: Option<String>,
}

/// One entry of the policy's top-k distribution. `action` is a coarse label
/// (e.g. `dahai:W`, `reach`, `pon`) — the exact tiles are in `reaction`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub action: String,
    #[serde(default)]
    pub prob: f64,
}

/// Response from `GET /v3/key` — the key's plan, expiry and live limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStatus {
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub expires_at: String,
    #[serde(default)]
    pub usage_today: u64,
    #[serde(default)]
    pub rpd: u64,
    /// Requests/minute. The server reports this as a float (e.g. `10.0`), so it
    /// is typed `f64` — deserializing it into an integer would fail the parse.
    #[serde(default)]
    pub rpm: f64,
    #[serde(default)]
    pub topk: u32,
}

/// One model the key's plan may use (`GET /v3/models`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub game: String,
    #[serde(default)]
    pub desc: String,
}

/// Response from `POST /v3/redeem`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeemResponse {
    /// The raw 32-char key — present **only** when a new key is minted
    /// (`extended == false`). Never re-shown on a renewal.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub key_last4: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub expires_at: String,
    /// `true` when time was stacked onto an existing key (no new key issued).
    #[serde(default)]
    pub extended: bool,
}

/// Response from `GET /healthz` — liveness + aggregate load.
///
/// The endpoint exposes aggregates only; nothing about the model registry
/// (model ids come from the authenticated, plan-filtered `/v3/models`).
/// `status` is `"degraded"` when any model worker is down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub status: String,
    /// Total pending + in-flight inference rows. Older servers sent a
    /// per-model map here — still accepted, summed to the same total, so a
    /// health check against a not-yet-updated server doesn't read as down.
    #[serde(default, deserialize_with = "queue_depth_total")]
    pub queue_depth: i64,
    /// `false` when any model worker is down. Absent on older servers;
    /// defaults to `true` so their `"ok"` doesn't render as degraded.
    #[serde(default = "default_true")]
    pub workers_alive: bool,
}

fn default_true() -> bool {
    true
}

/// Accept the aggregate integer or the legacy per-model map (summed).
fn queue_depth_total<'de, D>(de: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Depth {
        Total(i64),
        PerModel(BTreeMap<String, i64>),
    }
    Ok(match Depth::deserialize(de)? {
        Depth::Total(n) => n,
        Depth::PerModel(m) => m.values().sum(),
    })
}

#[derive(Serialize)]
struct ReactRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    player_id: u8,
    events: Vec<Value>,
}

#[derive(Serialize)]
struct RedeemRequest<'a> {
    code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    renew_key: Option<&'a str>,
}

/// Authenticated client bound to one server + key.
///
/// Each `ApiClient` builds its own `reqwest::Client`, and with it a fresh
/// connection pool — so **hold one and reuse it** rather than constructing one
/// per request, or every call pays a new TCP + TLS handshake. The built-in bot
/// keeps one alive across a game and only rebuilds it when the server URL or
/// key changes.
pub struct ApiClient {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl ApiClient {
    /// Build a client for `base_url` authenticating with `key`, optionally
    /// routing through `proxy` (see [`http_client`]; empty ⇒ direct). A
    /// trailing slash on the URL is tolerated.
    pub fn new(base_url: &str, key: &str, proxy: &str) -> Result<Self> {
        Ok(Self {
            base: normalize_base(base_url),
            key: key.trim().to_string(),
            http: http_client(REQUEST_TIMEOUT, proxy)?,
        })
    }

    /// `POST /v3/react` — the move for the final event of `events`. `model`
    /// `None`/empty lets the server pick its game default.
    ///
    /// Bounded by [`REACT_TIMEOUT`] rather than the client-wide
    /// [`REQUEST_TIMEOUT`]: this one blocks the bot's turn.
    pub async fn react(
        &self,
        model: Option<&str>,
        player_id: u8,
        events: Vec<Value>,
    ) -> Result<ReactResponse> {
        let url = format!("{}/v3/react", self.base);
        let body = ReactRequest {
            model: model.filter(|m| !m.is_empty()),
            player_id,
            events,
        };
        let resp = self
            .http
            .post(&url)
            .timeout(REACT_TIMEOUT)
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("POST /v3/react")?;
        let resp = check(resp, "react").await?;
        resp.json::<ReactResponse>()
            .await
            .context("parse /v3/react response")
    }

    /// `GET /v3/key` — the key's plan, expiry and live rate limits.
    pub async fn key_status(&self) -> Result<KeyStatus> {
        let url = format!("{}/v3/key", self.base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.key)
            .send()
            .await
            .context("GET /v3/key")?;
        let resp = check(resp, "key status").await?;
        resp.json::<KeyStatus>()
            .await
            .context("parse /v3/key response")
    }

    /// `GET /v3/models` — the models this key's plan may use.
    pub async fn models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/v3/models", self.base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.key)
            .send()
            .await
            .context("GET /v3/models")?;
        let resp = check(resp, "models").await?;
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            models: Vec<ModelInfo>,
        }
        Ok(resp
            .json::<Wrap>()
            .await
            .context("parse /v3/models response")?
            .models)
    }
}

/// `POST /v3/redeem` (no auth). By default mints a **new** key; pass
/// `renew_key` to stack time onto a key you already hold. `email` links minted
/// keys to an account (ignored when `renew_key` is set).
pub async fn redeem(
    base_url: &str,
    proxy: &str,
    code: &str,
    email: Option<&str>,
    renew_key: Option<&str>,
) -> Result<RedeemResponse> {
    let base = normalize_base(base_url);
    let http = http_client(REQUEST_TIMEOUT, proxy)?;
    let url = format!("{base}/v3/redeem");
    let body = RedeemRequest {
        code: code.trim(),
        email: email.map(str::trim).filter(|s| !s.is_empty()),
        renew_key: renew_key.map(str::trim).filter(|s| !s.is_empty()),
    };
    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("POST /v3/redeem")?;
    let resp = check(resp, "redeem").await?;
    resp.json::<RedeemResponse>()
        .await
        .context("parse /v3/redeem response")
}

/// `GET /healthz` (no auth) — liveness + aggregate load.
pub async fn health(base_url: &str, proxy: &str) -> Result<Health> {
    let base = normalize_base(base_url);
    let http = http_client(REQUEST_TIMEOUT, proxy)?;
    let url = format!("{base}/healthz");
    let resp = http.get(&url).send().await.context("GET /healthz")?;
    let resp = check(resp, "health").await?;
    resp.json::<Health>()
        .await
        .context("parse /healthz response")
}

/// Build the HTTP client all inference-server traffic goes through, honoring
/// the user's proxy setting (`bot.api.proxy`). `proxy` accepts
/// `http://`, `https://`, `socks5://` or `socks5h://` URLs (`socks5h` resolves
/// DNS on the proxy — what you want when the server's name doesn't resolve
/// locally); empty/whitespace ⇒ direct connection. Shared with
/// [`crate::bot::purchase`], so the whole `/v3` + `/paypal` surface follows
/// one setting.
pub(crate) fn http_client(timeout: Duration, proxy: &str) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    let proxy = proxy.trim();
    if !proxy.is_empty() {
        // Scheme match is case-insensitive (RFC 3986; the url crate lowercases
        // on parse anyway) — `HTTP://…` pasted from a provider dashboard works.
        const SCHEMES: [&str; 4] = ["http://", "https://", "socks5://", "socks5h://"];
        let scheme_ok = SCHEMES.iter().any(|s| {
            proxy
                .get(..s.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(s))
        });
        // Never echo the proxy string into errors: proxy URLs routinely carry
        // credentials (`socks5://user:pass@host`), and these messages end up
        // in toasts AND in the persisted session log (which users attach to
        // bug reports). The user just typed the value — no diagnostic value
        // in repeating it.
        if !scheme_ok {
            bail!("unsupported proxy scheme: use http://, https://, socks5:// or socks5h://");
        }
        let p = reqwest::Proxy::all(proxy).context("invalid proxy URL")?;
        builder = builder.proxy(p);
    }
    builder.build().context("build inference-API http client")
}

pub(crate) fn normalize_base(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// Turn a non-2xx response into a descriptive error, surfacing the server's
/// generic `{"error": "..."}` message and any `Retry-After` hint. Success
/// passes the response through untouched for the caller to deserialize.
/// Shared with the purchase client ([`crate::bot::purchase`]).
pub(crate) async fn check(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let raw = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| raw.chars().take(200).collect());
    let code = status.as_u16();
    match retry_after {
        Some(ra) => bail!("{what} failed: HTTP {code} — {msg} (retry after {ra}s)"),
        None => bail!("{what} failed: HTTP {code} — {msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_client_accepts_supported_proxy_schemes_and_empty() {
        for p in [
            "",
            "  ",
            "http://127.0.0.1:7890",
            "https://p:8443",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
            // Schemes are case-insensitive (RFC 3986).
            "HTTP://127.0.0.1:7890",
            "SOCKS5h://127.0.0.1:1080",
        ] {
            assert!(
                http_client(REQUEST_TIMEOUT, p).is_ok(),
                "proxy {p:?} should build"
            );
        }
    }

    #[test]
    fn http_client_rejects_unknown_proxy_scheme() {
        for p in [
            "ftp://x",
            "socks4://127.0.0.1:1080",
            "127.0.0.1:7890",
            "socks5:/bad",
        ] {
            let err = http_client(REQUEST_TIMEOUT, p).unwrap_err();
            assert!(
                format!("{err:#}").contains("unsupported proxy scheme"),
                "proxy {p:?} should be rejected with a clear message, got: {err:#}"
            );
        }
    }

    #[test]
    fn proxy_errors_never_echo_the_credentials() {
        // Proxy URLs often carry `user:pass@host`; the error text lands in
        // toasts and persisted logs, so it must not contain the secret.
        let err = http_client(REQUEST_TIMEOUT, "socks4://user:sekret@10.0.0.1:1080").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            !text.contains("sekret") && !text.contains("10.0.0.1"),
            "error text leaked the proxy string: {text}"
        );
    }

    #[test]
    fn normalize_trims_trailing_slash_and_space() {
        assert_eq!(normalize_base("http://host:8080/"), "http://host:8080");
        assert_eq!(normalize_base("  https://host  "), "https://host");
        assert_eq!(normalize_base("http://host:8080"), "http://host:8080");
    }

    #[test]
    fn react_request_omits_empty_model() {
        let body = ReactRequest {
            model: None,
            player_id: 0,
            events: vec![],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("model").is_none(), "model must be omitted when None");
        assert_eq!(v["player_id"], 0);
    }

    #[test]
    fn react_request_keeps_model_when_set() {
        let body = ReactRequest {
            model: Some("4p-ot2"),
            player_id: 2,
            events: vec![],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["model"], "4p-ot2");
        assert_eq!(v["player_id"], 2);
    }

    #[test]
    fn redeem_request_omits_blank_optionals() {
        let body = RedeemRequest {
            code: "abc",
            email: Some("  "),
            renew_key: None,
        };
        // Callers pass pre-filtered options; this mirrors the API's default
        // "mint a new key anonymously" path — code only.
        let filtered = RedeemRequest {
            code: body.code,
            email: body.email.map(str::trim).filter(|s| !s.is_empty()),
            renew_key: body.renew_key,
        };
        let v = serde_json::to_value(&filtered).unwrap();
        assert_eq!(v["code"], "abc");
        assert!(v.get("email").is_none());
        assert!(v.get("renew_key").is_none());
    }

    #[test]
    fn react_response_defaults_missing_fields() {
        let r: ReactResponse = serde_json::from_str("{}").unwrap();
        assert!(r.reaction.is_none());
        assert!(r.candidates.is_empty());
        assert!(r.model.is_none());
    }

    /// The live server returns `rpm` as a float (`10.0`); parsing it into an
    /// integer would fail, so `KeyStatus.rpm` is `f64`. Lock that in.
    #[test]
    fn key_status_parses_float_rpm() {
        let raw = r#"{"plan":"basic","expires_at":"2026-08-04 19:15:42","usage_today":3,"rpd":6000,"rpm":10.0,"topk":3}"#;
        let k: KeyStatus = serde_json::from_str(raw).unwrap();
        assert_eq!(k.plan, "basic");
        assert_eq!(k.usage_today, 3);
        assert_eq!(k.rpd, 6000);
        assert!((k.rpm - 10.0).abs() < 1e-9);
        assert_eq!(k.topk, 3);
    }

    /// The 2026-08 server update reshaped `/healthz`: `models` was dropped
    /// and `queue_depth` became a single aggregate integer. Parsing that
    /// integer into the old per-model map type was a hard serde error — a
    /// healthy server would have shown up as a failed health check.
    #[test]
    fn health_parses_aggregate_shape() {
        let raw = r#"{"status":"ok","queue_depth":3,"workers_alive":true}"#;
        let h: Health = serde_json::from_str(raw).unwrap();
        assert_eq!(h.status, "ok");
        assert_eq!(h.queue_depth, 3);
        assert!(h.workers_alive);

        let raw = r#"{"status":"degraded","queue_depth":0,"workers_alive":false}"#;
        let h: Health = serde_json::from_str(raw).unwrap();
        assert_eq!(h.status, "degraded");
        assert!(!h.workers_alive);
    }

    /// A not-yet-updated server still sends the legacy shape (per-model map
    /// plus a `models` list). It must keep parsing: the map sums to the same
    /// aggregate total, unknown fields are ignored, and the missing
    /// `workers_alive` defaults to alive rather than degraded.
    #[test]
    fn health_accepts_legacy_per_model_shape() {
        let raw = r#"{"status":"ok","models":["4p-x","3p-x"],"queue_depth":{"4p-x":2,"3p-x":1}}"#;
        let h: Health = serde_json::from_str(raw).unwrap();
        assert_eq!(h.status, "ok");
        assert_eq!(h.queue_depth, 3);
        assert!(h.workers_alive);

        // Bare minimum body — every field defaulted.
        let h: Health = serde_json::from_str("{}").unwrap();
        assert_eq!(h.queue_depth, 0);
        assert!(h.workers_alive);
    }

    #[test]
    fn models_wrapper_parses() {
        let raw = r#"{"models":[{"id":"4p-ot2","game":"4p","desc":"4p Mortal v4 (ot2), 192x40"},{"id":"3p-ot2","game":"3p","desc":"3p Mortal v4"}]}"#;
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            models: Vec<ModelInfo>,
        }
        let w: Wrap = serde_json::from_str(raw).unwrap();
        assert_eq!(w.models.len(), 2);
        assert_eq!(w.models[0].id, "4p-ot2");
        assert_eq!(w.models[1].game, "3p");
    }

    // ---- end-to-end against a local mock server ----

    use crate::bot::test_http::mock_http;

    /// Value of `name` in a raw HTTP request head, case-insensitively.
    fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
        req.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    /// The bearer key must travel in the `Authorization` header — never in the
    /// URL or query string, where proxies and server access logs would capture it.
    #[tokio::test]
    async fn react_authenticates_with_a_bearer_header() {
        let (base, served) = mock_http(vec![(
            "200 OK",
            r#"{"reaction":{"type":"none"},"candidates":[{"action":"none","prob":1.0}],"model":"4p-x"}"#
                .into(),
        )]);
        let client = ApiClient::new(&base, "SECRETKEY", "").unwrap();
        let resp = client
            .react(
                Some("4p-x"),
                2,
                vec![serde_json::json!({"type": "start_game"})],
            )
            .await
            .unwrap();
        assert_eq!(resp.model.as_deref(), Some("4p-x"));

        let reqs = served.join().unwrap();
        let r = &reqs[0];
        assert!(r.starts_with("POST /v3/react "), "wrong request line: {r}");
        assert_eq!(header(r, "authorization"), Some("Bearer SECRETKEY"));
        assert!(
            !r.lines().next().unwrap().contains("SECRETKEY"),
            "key must not appear in the request line: {r}"
        );
        assert!(r.contains(r#""player_id":2"#), "{r}");
        assert!(r.contains(r#""model":"4p-x""#), "{r}");
    }

    #[tokio::test]
    async fn key_status_and_models_authenticate_with_a_bearer_header() {
        let (base, served) = mock_http(vec![
            (
                "200 OK",
                r#"{"plan":"basic","expires_at":"2026-08-04","usage_today":1,"rpd":6000,"rpm":10.0,"topk":3}"#.into(),
            ),
            (
                "200 OK",
                r#"{"models":[{"id":"4p-x","game":"4p","desc":"d"}]}"#.into(),
            ),
        ]);
        let client = ApiClient::new(&base, "SECRETKEY", "").unwrap();
        assert_eq!(client.key_status().await.unwrap().plan, "basic");
        assert_eq!(client.models().await.unwrap()[0].id, "4p-x");

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("GET /v3/key "), "{}", reqs[0]);
        assert_eq!(header(&reqs[0], "authorization"), Some("Bearer SECRETKEY"));
        assert!(reqs[1].starts_with("GET /v3/models "), "{}", reqs[1]);
        assert_eq!(header(&reqs[1], "authorization"), Some("Bearer SECRETKEY"));
    }

    /// `redeem` and `health` are the unauthenticated endpoints — a buyer calls
    /// them before they hold a key. Sending one would leak it needlessly.
    #[tokio::test]
    async fn redeem_and_health_send_no_authorization_header() {
        let (base, served) = mock_http(vec![
            (
                "200 OK",
                r#"{"key":"K","key_last4":"cdef","plan":"basic","expires_at":"2026-08-04","extended":false}"#.into(),
            ),
            (
                "200 OK",
                r#"{"status":"ok","queue_depth":0,"workers_alive":true}"#.into(),
            ),
        ]);
        let r = redeem(&base, "", " CODE ", Some(" "), None).await.unwrap();
        assert_eq!(r.key.as_deref(), Some("K"));
        assert_eq!(health(&base, "").await.unwrap().status, "ok");

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /v3/redeem "), "{}", reqs[0]);
        assert!(header(&reqs[0], "authorization").is_none());
        // Blank optionals are dropped, and the code is trimmed.
        assert!(reqs[0].contains(r#""code":"CODE""#), "{}", reqs[0]);
        assert!(!reqs[0].contains("email"), "{}", reqs[0]);
        assert!(reqs[1].starts_with("GET /healthz "), "{}", reqs[1]);
        assert!(header(&reqs[1], "authorization").is_none());
    }

    /// End-to-end through a mock SOCKS5 proxy: greeting → no-auth → CONNECT
    /// (domain ATYP, i.e. DNS resolved BY the proxy — `socks5h`) → success →
    /// plain HTTP inside the tunnel. Locks in that socks support is actually
    /// compiled into reqwest — without the `socks` feature this whole path
    /// fails at `Proxy::all` and the setting would silently be unusable.
    #[tokio::test]
    async fn socks5h_proxy_tunnels_the_request_and_resolves_dns_remotely() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).unwrap();
            assert!(
                n >= 2 && buf[0] == 0x05,
                "not a SOCKS5 greeting: {:?}",
                &buf[..n]
            );
            s.write_all(&[0x05, 0x00]).unwrap(); // no-auth accepted
            let n = s.read(&mut buf).unwrap();
            assert!(n >= 4 && buf[1] == 0x01, "not a CONNECT: {:?}", &buf[..n]);
            // ATYP 0x03 = domain name: the client did NOT resolve locally.
            assert_eq!(buf[3], 0x03, "expected domain ATYP, got {:#x}", buf[3]);
            s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .unwrap();
            // From here the socket IS the tunnel — speak plain HTTP on it.
            let mut req = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = s.read(&mut tmp).unwrap();
                req.extend_from_slice(&tmp[..n]);
                if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"status":"ok","queue_depth":0,"workers_alive":true}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).to_string()
        });
        // The target host doesn't exist — the request can only succeed by
        // going through the mock proxy.
        let h = health("http://ot.invalid:8080", &format!("socks5h://{addr}"))
            .await
            .unwrap();
        assert_eq!(h.status, "ok");
        let req = served.join().unwrap();
        assert!(req.starts_with("GET /healthz HTTP/1.1"), "{req}");
    }

    /// Plain HTTP proxying is the same request with an absolute URI in the
    /// request line — `mock_http` can play the proxy directly.
    #[tokio::test]
    async fn http_proxy_receives_the_absolute_form_request() {
        let (proxy_base, served) = mock_http(vec![(
            "200 OK",
            r#"{"status":"ok","queue_depth":0,"workers_alive":true}"#.into(),
        )]);
        let h = health("http://ot.invalid:8080", &proxy_base).await.unwrap();
        assert_eq!(h.status, "ok");
        let reqs = served.join().unwrap();
        assert!(
            reqs[0].starts_with("GET http://ot.invalid:8080/healthz"),
            "expected an absolute-form request line, got: {}",
            reqs[0]
        );
    }

    /// A non-2xx surfaces the server's `error` message and the `Retry-After` hint.
    #[tokio::test]
    async fn http_error_carries_the_server_message() {
        let (base, served) = mock_http(vec![(
            "429 Too Many Requests",
            r#"{"error":"rate limited"}"#.into(),
        )]);
        let client = ApiClient::new(&base, "K", "").unwrap();
        let err = client.key_status().await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("429"), "{msg}");
        assert!(msg.contains("rate limited"), "{msg}");
        let _ = served.join();
    }
}
