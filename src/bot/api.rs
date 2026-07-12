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

/// Response from `GET /healthz` — liveness + per-model queue depth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub queue_depth: BTreeMap<String, i64>,
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
    /// Build a client for `base_url` authenticating with `key`. A trailing
    /// slash on the URL is tolerated.
    pub fn new(base_url: &str, key: &str) -> Result<Self> {
        Self::new_with_proxy(base_url, key, false)
    }

    /// Build a client, optionally honoring the operating system proxy.
    pub fn new_with_proxy(base_url: &str, key: &str, use_system_proxy: bool) -> Result<Self> {
        Ok(Self {
            base: normalize_base(base_url),
            key: key.trim().to_string(),
            http: build_http(use_system_proxy)?,
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
    code: &str,
    email: Option<&str>,
    renew_key: Option<&str>,
) -> Result<RedeemResponse> {
    redeem_with_proxy(base_url, code, email, renew_key, false).await
}

pub async fn redeem_with_proxy(
    base_url: &str,
    code: &str,
    email: Option<&str>,
    renew_key: Option<&str>,
    use_system_proxy: bool,
) -> Result<RedeemResponse> {
    let base = normalize_base(base_url);
    let http = build_http(use_system_proxy)?;
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

/// `GET /healthz` (no auth) — liveness + per-model queue depth.
pub async fn health(base_url: &str) -> Result<Health> {
    health_with_proxy(base_url, false).await
}

pub async fn health_with_proxy(base_url: &str, use_system_proxy: bool) -> Result<Health> {
    let base = normalize_base(base_url);
    let http = build_http(use_system_proxy)?;
    let url = format!("{base}/healthz");
    let resp = http.get(&url).send().await.context("GET /healthz")?;
    let resp = check(resp, "health").await?;
    resp.json::<Health>()
        .await
        .context("parse /healthz response")
}

fn build_http(use_system_proxy: bool) -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
    let builder = configure_proxy(builder, use_system_proxy)?;
    builder.build().context("build inference-API http client")
}

pub(crate) fn configure_proxy(
    builder: reqwest::ClientBuilder,
    use_system_proxy: bool,
) -> Result<reqwest::ClientBuilder> {
    if !use_system_proxy {
        return Ok(builder.no_proxy());
    }
    let Some(proxy) = system_proxy_endpoint() else {
        return Ok(builder.no_proxy());
    };
    Ok(builder.proxy(reqwest::Proxy::all(&proxy).with_context(|| {
        format!("invalid system proxy endpoint `{proxy}`")
    })?))
}

fn system_proxy_endpoint() -> Option<String> {
    #[cfg(windows)]
    if let Some(proxy) = windows_system_proxy_endpoint() {
        return Some(proxy);
    }

    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok())
        .and_then(|v| normalize_proxy_endpoint(&v))
}

#[cfg(windows)]
fn windows_system_proxy_endpoint() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled = settings.get_value::<u32, _>("ProxyEnable").ok()? != 0;
    if !enabled {
        return None;
    }
    let raw = settings.get_value::<String, _>("ProxyServer").ok()?;
    parse_windows_proxy_server(&raw).and_then(|v| normalize_proxy_endpoint(&v))
}

#[cfg(windows)]
fn parse_windows_proxy_server(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains(';') && !trimmed.contains('=') {
        return Some(trimmed.to_string());
    }
    let mut fallback = None;
    for part in trimmed.split(';') {
        let (scheme, value) = part.split_once('=')?;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match scheme.trim().to_ascii_lowercase().as_str() {
            "https" => return Some(value.to_string()),
            "http" => fallback = Some(value.to_string()),
            _ => {}
        }
    }
    fallback
}

fn normalize_proxy_endpoint(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("://") {
        Some(trimmed.to_string())
    } else {
        Some(format!("http://{trimmed}"))
    }
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
        let client = ApiClient::new(&base, "SECRETKEY").unwrap();
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
        let client = ApiClient::new(&base, "SECRETKEY").unwrap();
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
            ("200 OK", r#"{"status":"ok","models":["4p-x"]}"#.into()),
        ]);
        let r = redeem(&base, " CODE ", Some(" "), None).await.unwrap();
        assert_eq!(r.key.as_deref(), Some("K"));
        assert_eq!(health(&base).await.unwrap().status, "ok");

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /v3/redeem "), "{}", reqs[0]);
        assert!(header(&reqs[0], "authorization").is_none());
        // Blank optionals are dropped, and the code is trimmed.
        assert!(reqs[0].contains(r#""code":"CODE""#), "{}", reqs[0]);
        assert!(!reqs[0].contains("email"), "{}", reqs[0]);
        assert!(reqs[1].starts_with("GET /healthz "), "{}", reqs[1]);
        assert!(header(&reqs[1], "authorization").is_none());
    }

    /// A non-2xx surfaces the server's `error` message and the `Retry-After` hint.
    #[tokio::test]
    async fn http_error_carries_the_server_message() {
        let (base, served) = mock_http(vec![(
            "429 Too Many Requests",
            r#"{"error":"rate limited"}"#.into(),
        )]);
        let client = ApiClient::new(&base, "K").unwrap();
        let err = client.key_status().await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("429"), "{msg}");
        assert!(msg.contains("rate limited"), "{msg}");
        let _ = served.join();
    }
}
