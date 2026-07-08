//! Self-serve purchase client for the inference API's PayPal endpoints
//! (`/paypal/*`, see `native_bot/API.md` §13).
//!
//! Every purchase is a three-step handshake: **create → approve → collect**.
//! [`create_order`] / [`create_subscription`] start one and return an
//! `approve_url` (opened in the buyer's browser) plus a one-time
//! `claim_secret`. While the buyer approves on PayPal, the app polls
//! [`order_result`] / [`subscription_result`] with `{id, claim}` until the
//! status flips from `pending` to `ready` — which carries the redeem code
//! (one-time purchase) or the API key itself (subscription).
//!
//! All of these endpoints are **unauthenticated** (the buyer is acquiring a
//! key, so they don't hold one yet) but per-IP rate limited. Amounts are
//! server-owned: only a product id crosses the wire, never a price.
//!
//! The polling loop and purchase state machine live in the frontend
//! (`purchaseStore`); this module is just the typed HTTP layer, mirroring
//! [`crate::bot::api`]. `status` fields stay plain strings on purpose — the
//! API doc asks clients to read fields defensively, and an unknown status
//! must degrade gracefully rather than fail the JSON parse.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::api::{check, normalize_base};

/// `create-*` calls block on PayPal upstream (the server creates the order /
/// subscription there before answering), so give them more headroom than the
/// inference client's tight game-path timeout. Result polls share the client;
/// they are idempotent and cheap, so the generous ceiling is harmless.
const PURCHASE_TIMEOUT: Duration = Duration::from_secs(20);

/// `POST /paypal/create-order` result — a pending one-time purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedOrder {
    pub order_id: String,
    /// PayPal checkout page for the buyer's browser.
    pub approve_url: String,
    /// One-time secret gating the `order-result` poll. Shown only here —
    /// keep it in memory for the poll loop; it is never re-issued.
    pub claim_secret: String,
}

/// `POST /paypal/create-subscription` result — a pending subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedSubscription {
    pub subscription_id: String,
    pub approve_url: String,
    pub claim_secret: String,
}

/// One poll of `POST /paypal/order-result`.
///
/// `status`: `pending` (keep polling) / `ready` (`code`, `plan`, `days` set)
/// / `delivered` (retrieval window passed — the code was emailed) /
/// `refunded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResult {
    pub status: String,
    /// The prepaid redeem code, present only on `ready`. Feed it to
    /// `POST /v3/redeem` to turn it into an API key.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub days: Option<u64>,
}

/// One poll of `POST /paypal/subscription-result`.
///
/// `status`: `pending` / `ready` (`key` set — the API key itself, no redeem
/// step) / `delivered` (emailed) / `cancelled` / `expired` / `suspended`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionResult {
    pub status: String,
    /// The auto-renewing API key, present only on `ready`.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub next_billing: Option<String>,
}

#[derive(Serialize)]
struct CreateRequest<'a> {
    product: &'a str,
}

#[derive(Serialize)]
struct OrderResultRequest<'a> {
    order_id: &'a str,
    claim: &'a str,
}

#[derive(Serialize)]
struct SubscriptionResultRequest<'a> {
    subscription_id: &'a str,
    claim: &'a str,
}

/// `POST /paypal/create-order` (no auth) — start a one-time purchase for
/// `product` (an operator-defined id, e.g. `pro-30`). **Not idempotent**:
/// each call opens a new PayPal order, so call once per purchase and reuse
/// the returned ids.
pub async fn create_order(base_url: &str, product: &str) -> Result<CreatedOrder> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/create-order");
    let resp = build_http()?
        .post(&url)
        .json(&CreateRequest {
            product: product.trim(),
        })
        .send()
        .await
        .context("POST /paypal/create-order")?;
    let resp = check(resp, "create order").await?;
    resp.json::<CreatedOrder>()
        .await
        .context("parse /paypal/create-order response")
}

/// `POST /paypal/order-result` (no auth) — poll a one-time purchase.
/// Idempotent and safe to repeat every few seconds until a terminal status.
/// A wrong `claim` is a `404` and counts toward the per-IP failure guard, so
/// never retry with guessed secrets.
pub async fn order_result(base_url: &str, order_id: &str, claim: &str) -> Result<OrderResult> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/order-result");
    let resp = build_http()?
        .post(&url)
        .json(&OrderResultRequest { order_id, claim })
        .send()
        .await
        .context("POST /paypal/order-result")?;
    let resp = check(resp, "order result").await?;
    resp.json::<OrderResult>()
        .await
        .context("parse /paypal/order-result response")
}

/// `POST /paypal/create-subscription` (no auth) — start a recurring
/// subscription for `product` (e.g. `pro-monthly`). Same non-idempotency
/// caveat as [`create_order`].
pub async fn create_subscription(base_url: &str, product: &str) -> Result<CreatedSubscription> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/create-subscription");
    let resp = build_http()?
        .post(&url)
        .json(&CreateRequest {
            product: product.trim(),
        })
        .send()
        .await
        .context("POST /paypal/create-subscription")?;
    let resp = check(resp, "create subscription").await?;
    resp.json::<CreatedSubscription>()
        .await
        .context("parse /paypal/create-subscription response")
}

/// `POST /paypal/subscription-result` (no auth) — poll a subscription. On
/// `ready` the response carries the API key directly (no redeem step).
pub async fn subscription_result(
    base_url: &str,
    subscription_id: &str,
    claim: &str,
) -> Result<SubscriptionResult> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/subscription-result");
    let resp = build_http()?
        .post(&url)
        .json(&SubscriptionResultRequest {
            subscription_id,
            claim,
        })
        .send()
        .await
        .context("POST /paypal/subscription-result")?;
    let resp = check(resp, "subscription result").await?;
    resp.json::<SubscriptionResult>()
        .await
        .context("parse /paypal/subscription-result response")
}

fn build_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(PURCHASE_TIMEOUT)
        .build()
        .context("build purchase http client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    // ---- serde shapes (fake data throughout — no real PayPal traffic) ----

    #[test]
    fn created_order_parses() {
        let raw = r#"{"order_id":"5O190127TN364715T",
                      "approve_url":"https://www.paypal.com/checkoutnow?token=5O190127TN364715T",
                      "claim_secret":"claimclaimclaimclaimclaimclaim12"}"#;
        let o: CreatedOrder = serde_json::from_str(raw).unwrap();
        assert_eq!(o.order_id, "5O190127TN364715T");
        assert!(o.approve_url.starts_with("https://www.paypal.com/"));
        assert_eq!(o.claim_secret.len(), 32);
    }

    #[test]
    fn order_result_pending_has_no_code() {
        let r: OrderResult = serde_json::from_str(r#"{"status":"pending"}"#).unwrap();
        assert_eq!(r.status, "pending");
        assert!(r.code.is_none());
        assert!(r.plan.is_none());
        assert!(r.days.is_none());
    }

    #[test]
    fn order_result_ready_carries_code_plan_days() {
        let raw = r#"{"status":"ready","code":"ab12cd34ef56gh78","plan":"pro","days":30}"#;
        let r: OrderResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.status, "ready");
        assert_eq!(r.code.as_deref(), Some("ab12cd34ef56gh78"));
        assert_eq!(r.plan.as_deref(), Some("pro"));
        assert_eq!(r.days, Some(30));
    }

    #[test]
    fn subscription_result_ready_carries_key() {
        let raw = r#"{"status":"ready","key":"k0000000000000000000000000000001",
                      "plan":"pro","next_billing":"2026-08-01T00:00:00Z"}"#;
        let r: SubscriptionResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.status, "ready");
        assert_eq!(r.key.as_deref(), Some("k0000000000000000000000000000001"));
        assert_eq!(r.next_billing.as_deref(), Some("2026-08-01T00:00:00Z"));
    }

    /// Unknown / future statuses must parse (read fields defensively per the
    /// API doc) — the frontend decides how to render them.
    #[test]
    fn unknown_status_still_parses() {
        let r: OrderResult = serde_json::from_str(r#"{"status":"on_hold"}"#).unwrap();
        assert_eq!(r.status, "on_hold");
        let s: SubscriptionResult = serde_json::from_str(r#"{"status":"suspended"}"#).unwrap();
        assert_eq!(s.status, "suspended");
        assert!(s.key.is_none());
    }

    #[test]
    fn request_bodies_have_expected_shape() {
        let v = serde_json::to_value(CreateRequest { product: "pro-30" }).unwrap();
        assert_eq!(v, serde_json::json!({"product": "pro-30"}));
        let v = serde_json::to_value(OrderResultRequest {
            order_id: "OID",
            claim: "SECRET",
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({"order_id": "OID", "claim": "SECRET"}));
        let v = serde_json::to_value(SubscriptionResultRequest {
            subscription_id: "I-BW452GLLEP1G",
            claim: "SECRET",
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"subscription_id": "I-BW452GLLEP1G", "claim": "SECRET"})
        );
    }

    // ---- end-to-end against a local mock server ----

    /// Minimal HTTP mock: serves one canned JSON response per accepted
    /// connection, captures each raw request, then returns them all.
    fn mock_http(responses: Vec<(&'static str, String)>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status_line, body) in responses {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let n = sock.read(&mut tmp).unwrap();
                    assert!(n > 0, "client hung up mid-request");
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let want: usize = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        while buf.len() - (pos + 4) < want {
                            let n = sock.read(&mut tmp).unwrap();
                            assert!(n > 0, "client hung up mid-body");
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&buf).into_owned());
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn create_then_poll_order_roundtrip() {
        let (base, served) = mock_http(vec![
            (
                "200 OK",
                r#"{"order_id":"OID1","approve_url":"https://paypal.example/approve","claim_secret":"S1"}"#.into(),
            ),
            ("200 OK", r#"{"status":"pending"}"#.into()),
            (
                "200 OK",
                r#"{"status":"ready","code":"code1234code5678","plan":"pro","days":30}"#.into(),
            ),
        ]);

        let created = create_order(&format!("{base}/"), "pro-30").await.unwrap();
        assert_eq!(created.order_id, "OID1");
        assert_eq!(created.claim_secret, "S1");

        let pending = order_result(&base, &created.order_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(pending.status, "pending");

        let ready = order_result(&base, &created.order_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(ready.code.as_deref(), Some("code1234code5678"));

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /paypal/create-order HTTP/1.1"));
        assert!(reqs[0].contains(r#"{"product":"pro-30"}"#));
        assert!(reqs[1].starts_with("POST /paypal/order-result HTTP/1.1"));
        assert!(reqs[1].contains(r#""order_id":"OID1""#));
        assert!(reqs[1].contains(r#""claim":"S1""#));
    }

    #[tokio::test]
    async fn subscription_roundtrip_returns_key_directly() {
        let (base, served) = mock_http(vec![
            (
                "200 OK",
                r#"{"subscription_id":"I-1","approve_url":"https://paypal.example/sub","claim_secret":"S2"}"#.into(),
            ),
            (
                "200 OK",
                r#"{"status":"ready","key":"k0000000000000000000000000000002","plan":"pro","next_billing":"2026-08-06T00:00:00Z"}"#.into(),
            ),
        ]);

        let created = create_subscription(&base, "pro-monthly").await.unwrap();
        assert_eq!(created.subscription_id, "I-1");

        let ready = subscription_result(&base, &created.subscription_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(
            ready.key.as_deref(),
            Some("k0000000000000000000000000000002")
        );

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /paypal/create-subscription HTTP/1.1"));
        assert!(reqs[0].contains(r#"{"product":"pro-monthly"}"#));
        assert!(reqs[1].contains(r#""subscription_id":"I-1""#));
    }

    /// Server error bodies surface through `check` with the endpoint label
    /// and the server's generic message — what the frontend shows verbatim.
    #[tokio::test]
    async fn create_order_surfaces_server_error() {
        let (base, served) = mock_http(vec![(
            "400 Bad Request",
            r#"{"error":"unknown product"}"#.into(),
        )]);
        let err = create_order(&base, "nope-99").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("create order failed"), "got: {msg}");
        assert!(msg.contains("HTTP 400"), "got: {msg}");
        assert!(msg.contains("unknown product"), "got: {msg}");
        served.join().unwrap();
    }

    /// A wrong claim is a 404 — the caller treats it as terminal (never
    /// retry with guessed secrets; it trips the per-IP failure guard).
    #[tokio::test]
    async fn order_result_wrong_claim_is_404() {
        let (base, served) = mock_http(vec![("404 Not Found", r#"{"error":"not found"}"#.into())]);
        let err = order_result(&base, "OID1", "WRONG").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 404"), "got: {msg}");
        served.join().unwrap();
    }
}
