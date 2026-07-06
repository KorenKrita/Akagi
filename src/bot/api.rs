//! HTTP client for the remote inference API (`/v3/*`, see
//! `native_bot/API.md`).
//!
//! The service is **stateless**: every [`ApiClient::react`] call uploads the
//! current kyoku's mjai event stream (from the bot's seat perspective, ending
//! at the decision point) and gets back the move to play. This module is a thin
//! typed wrapper around those endpoints — the mjai stream shaping / censoring
//! lives in [`crate::bot::native`], the caller.
//!
//! Two consumers:
//! - the built-in bot ([`crate::bot::native::ApiNativeBot`]) calls
//!   [`ApiClient::react`] at each decision point;
//! - the IPC layer ([`crate::ipc::commands`]) calls [`redeem`],
//!   [`ApiClient::key_status`], [`ApiClient::models`] and [`health`] so the
//!   frontend can redeem codes and inspect a key.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Per-request timeout. A `react` call sits on a live game's critical path, so
/// keep it tight enough that a hung server falls back to the local model
/// promptly rather than making the bot miss its turn.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

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

/// Authenticated client bound to one server + key. Cheap to build (the inner
/// `reqwest::Client` owns a shared connection pool); the built-in bot holds one
/// for the lifetime of a game.
pub struct ApiClient {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl ApiClient {
    /// Build a client for `base_url` authenticating with `key`. A trailing
    /// slash on the URL is tolerated.
    pub fn new(base_url: &str, key: &str) -> Result<Self> {
        Ok(Self {
            base: normalize_base(base_url),
            key: key.trim().to_string(),
            http: build_http()?,
        })
    }

    /// `POST /v3/react` — the move for the final event of `events`. `model`
    /// `None`/empty lets the server pick its game default.
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
    let base = normalize_base(base_url);
    let http = build_http()?;
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
    let base = normalize_base(base_url);
    let http = build_http()?;
    let url = format!("{base}/healthz");
    let resp = http.get(&url).send().await.context("GET /healthz")?;
    let resp = check(resp, "health").await?;
    resp.json::<Health>()
        .await
        .context("parse /healthz response")
}

fn build_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("build inference-API http client")
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
        let raw = r#"{"models":[{"id":"4p-ot2","game":"4p","desc":"4p Mortal v4 (ot2), 192x40"},{"id":"3p-ot","game":"3p","desc":"3p Mortal v4"}]}"#;
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
}
