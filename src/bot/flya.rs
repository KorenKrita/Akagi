//! Client for FlyA's public beta API (`flya-test-api-v1`).

use crate::bot::api::{
    check, configure_proxy, Candidate, Health, KeyStatus, ModelInfo, ReactResponse,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DECISION_TIMEOUT: Duration = Duration::from_secs(10);

pub struct FlyApiClient {
    base: String,
    key: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct DecisionRequest<'a> {
    request_id: &'a str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_id: Option<&'a str>,
    state: StateEnvelope,
    deadline_ms: u64,
}

#[derive(Serialize)]
struct StateEnvelope {
    schema: &'static str,
    rule_line: &'static str,
    viewer_seat: u8,
    source: Value,
    from_seq: usize,
    to_seq: usize,
    events: Vec<Value>,
}

#[derive(Deserialize)]
struct DecisionResponse {
    #[serde(default)]
    model_id: Option<String>,
    attempt: Attempt,
}

#[derive(Deserialize)]
struct Attempt {
    status: String,
    #[serde(default)]
    selected_action_id: Option<u64>,
    #[serde(default)]
    action: Option<Value>,
    #[serde(default)]
    actions: Vec<FlyCandidate>,
}

#[derive(Deserialize)]
struct FlyCandidate {
    action_id: u64,
    action: Value,
    #[serde(default)]
    probability: f64,
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    models: Vec<FlyModel>,
}

#[derive(Deserialize)]
struct FlyModel {
    model_id: String,
    #[serde(default)]
    display_name: String,
    rule_line: String,
    #[serde(default)]
    available: bool,
}

impl FlyApiClient {
    pub fn new_with_proxy(base_url: &str, key: &str, use_system_proxy: bool) -> Result<Self> {
        let base = normalize_base(base_url)?;
        let key = key.trim();
        if key.is_empty() {
            bail!("FlyA API key is required");
        }
        let builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
        let http = configure_proxy(builder, use_system_proxy)?
            .build()
            .context("build FlyA HTTP client")?;
        Ok(Self {
            base,
            key: key.to_string(),
            http,
        })
    }

    pub async fn decision(
        &self,
        model: Option<&str>,
        player_id: u8,
        num_players: u8,
        session_id: &str,
        request_id: &str,
        events: Vec<Value>,
    ) -> Result<ReactResponse> {
        let target = last_discard_actor(&events);
        let state = StateEnvelope {
            schema: "flya-mahjong-events-v2",
            rule_line: if num_players == 3 {
                "riichi3p"
            } else {
                "riichi4p"
            },
            viewer_seat: player_id,
            source: json!({ "kind": "observed" }),
            from_seq: 0,
            to_seq: events.len(),
            events,
        };
        let body = DecisionRequest {
            request_id,
            session_id,
            model_id: model.filter(|m| !m.trim().is_empty()),
            state,
            deadline_ms: DECISION_TIMEOUT.as_millis() as u64,
        };
        let resp = self
            .http
            .post(format!("{}/decision", self.base))
            .timeout(DECISION_TIMEOUT + Duration::from_secs(1))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("POST FlyA /decision")?;
        let resp = check(resp, "FlyA decision").await?;
        let parsed = resp
            .json::<DecisionResponse>()
            .await
            .context("parse FlyA /decision response")?;
        if parsed.attempt.status != "success" {
            bail!("FlyA decision attempt ended with {}", parsed.attempt.status);
        }
        let selected_id = parsed
            .attempt
            .selected_action_id
            .context("FlyA success response omitted selected_action_id")?;
        let selected = parsed
            .attempt
            .action
            .context("FlyA success response omitted action")?;
        let matching = parsed
            .attempt
            .actions
            .iter()
            .find(|c| c.action_id == selected_id)
            .context("FlyA selected_action_id is absent from actions")?;
        if matching.action != selected {
            bail!("FlyA selected action does not match its action_id");
        }

        let reaction = fly_action_to_mjai(&selected, player_id, target)?;
        let mut candidates = parsed.attempt.actions;
        candidates.sort_by_key(|c| if c.action_id == selected_id { 0 } else { 1 });
        let candidates = candidates
            .into_iter()
            .filter_map(|c| {
                fly_action_label(&c.action).map(|action| Candidate {
                    action,
                    prob: c.probability,
                })
            })
            .collect();
        Ok(ReactResponse {
            reaction: Some(reaction),
            candidates,
            model: parsed.model_id,
        })
    }

    /// `/quota` both activates a fresh FlyA key and reports its status.
    pub async fn key_status(&self) -> Result<KeyStatus> {
        let value = self.get_json("quota").await?;
        let kind = value
            .get("key_kind")
            .and_then(Value::as_str)
            .unwrap_or("FlyA");
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let used = decimal_floor(value.get("quota_used"));
        let total = decimal_floor(value.get("quota_total"));
        Ok(KeyStatus {
            plan: format!("FlyA {kind} ({status})"),
            expires_at: value
                .get("expires_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            usage_today: used,
            rpd: total,
            rpm: 0.0,
            topk: 0,
        })
    }

    pub async fn models(&self) -> Result<Vec<ModelInfo>> {
        // `/quota` is FlyA's activation endpoint. Calling it first makes a
        // newly issued (frozen) key usable before `/models` is requested.
        self.key_status().await?;
        let value = self.get_json("models").await?;
        let parsed: ModelsResponse = serde_json::from_value(value).context("parse FlyA models")?;
        Ok(parsed
            .models
            .into_iter()
            .filter(|m| m.available)
            .map(|m| ModelInfo {
                id: m.model_id,
                game: if m.rule_line == "riichi3p" {
                    "3p"
                } else {
                    "4p"
                }
                .to_string(),
                desc: m.display_name,
            })
            .collect())
    }

    pub async fn health(&self) -> Result<Health> {
        let models = self.models().await?;
        Ok(Health {
            status: if models.is_empty() { "degraded" } else { "ok" }.to_string(),
            models: models.into_iter().map(|m| m.id).collect(),
            queue_depth: BTreeMap::new(),
        })
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/{path}", self.base))
            .bearer_auth(&self.key)
            .send()
            .await
            .with_context(|| format!("GET FlyA /{path}"))?;
        let resp = check(resp, &format!("FlyA {path}")).await?;
        resp.json::<Value>()
            .await
            .with_context(|| format!("parse FlyA /{path} response"))
    }
}

fn normalize_base(raw: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(raw.trim()).context("invalid FlyA server URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("FlyA server URL must use http:// or https://");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/" | "/beta/v1" | "/beta/v1/")
    {
        bail!("FlyA server URL must be an origin or end with /beta/v1");
    }
    url.set_path("/beta/v1");
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn decimal_floor(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_str)
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn last_discard_actor(events: &[Value]) -> Option<u8> {
    events.iter().rev().find_map(|event| {
        matches!(
            event.get("type").and_then(Value::as_str),
            Some("dahai") | Some("dealer_opening_dahai")
        )
        .then(|| event.get("actor").and_then(Value::as_u64).map(|v| v as u8))
        .flatten()
    })
}

fn fly_action_to_mjai(action: &Value, seat: u8, target: Option<u8>) -> Result<Value> {
    let kind = action
        .get("type")
        .and_then(Value::as_str)
        .context("FlyA action omitted type")?;
    let pai = || {
        action
            .get("pai")
            .cloned()
            .context("FlyA action omitted pai")
    };
    let consumed = || {
        action
            .get("consumed")
            .cloned()
            .context("FlyA action omitted consumed")
    };
    Ok(match kind {
        "dahai" => {
            json!({"type":"dahai","actor":seat,"pai":pai()?,"tsumogiri":action.get("tsumogiri").and_then(Value::as_bool).unwrap_or(false)})
        }
        "dealer_opening_dahai" => {
            json!({"type":"dahai","actor":seat,"pai":pai()?,"tsumogiri":false})
        }
        "riichi_dahai" | "dealer_opening_riichi_dahai" => {
            json!({"type":"reach","actor":seat,"pai":pai()?})
        }
        "chi" | "pon" | "daiminkan" => {
            json!({"type":kind,"actor":seat,"target":target.context("FlyA call action has no preceding discard")?,"pai":pai()?,"consumed":consumed()?})
        }
        "ankan" => json!({"type":"ankan","actor":seat,"consumed":consumed()?}),
        "kakan" => json!({"type":"kakan","actor":seat,"pai":pai()?,"consumed":consumed()?}),
        "kita" => json!({"type":"kita","actor":seat,"pai":"N"}),
        "tsumo" => json!({"type":"hora","actor":seat,"target":seat}),
        "ron" => {
            json!({"type":"hora","actor":seat,"target":action.get("target").and_then(Value::as_u64).context("FlyA ron omitted target")?})
        }
        "kyushukyuhai" => json!({"type":"ryukyoku"}),
        "pass_all" => json!({"type":"none"}),
        other => bail!("unsupported FlyA action type `{other}`"),
    })
}

fn fly_action_label(action: &Value) -> Option<String> {
    let kind = action.get("type")?.as_str()?;
    match kind {
        "dahai" | "dealer_opening_dahai" => Some(format!("dahai:{}", action.get("pai")?.as_str()?)),
        "riichi_dahai" | "dealer_opening_riichi_dahai" => Some("reach".into()),
        "pass_all" => Some("none".into()),
        "daiminkan" | "ankan" | "kakan" => Some("kan".into()),
        "kita" => Some("nukidora".into()),
        "ron" | "tsumo" => Some("hora".into()),
        "kyushukyuhai" => Some("ryukyoku".into()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_origin_and_versioned_base() {
        assert_eq!(
            normalize_base("https://api.nashout.com/").unwrap(),
            "https://api.nashout.com/beta/v1"
        );
        assert_eq!(
            normalize_base("https://api.nashout.com/beta/v1").unwrap(),
            "https://api.nashout.com/beta/v1"
        );
        assert!(normalize_base("https://user@example.com").is_err());
        assert!(normalize_base("https://api.nashout.com/other").is_err());
    }

    #[test]
    fn converts_flya_actions_to_mjai() {
        let v = fly_action_to_mjai(
            &json!({"type":"riichi_dahai","pai":"5pr","tsumogiri":false}),
            2,
            None,
        )
        .unwrap();
        assert_eq!(v, json!({"type":"reach","actor":2,"pai":"5pr"}));
        let v = fly_action_to_mjai(
            &json!({"type":"pon","pai":"E","consumed":["E","E"]}),
            2,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"type":"pon","actor":2,"target":1,"pai":"E","consumed":["E","E"]})
        );
    }
}
