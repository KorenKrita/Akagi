//! Self-serve purchase client for the inference API's PayPal endpoints
//! (`/paypal/*`), as published by the inference server's own API documentation.
//!
//! Every purchase is a three-step handshake: **create → approve → collect**.
//! [`create_order`] / [`create_subscription`] start one and return an
//! `approve_url` (opened in the buyer's browser) plus a one-time
//! `claim_secret`. While the buyer approves on PayPal, the app polls
//! [`order_result`] / [`subscription_result`] with `{id, claim}` until the
//! status flips from `pending` to `ready` — which carries either an API key
//! or a redeem code, depending on the purchase (see below).
//!
//! A subscription always resolves straight to a key. A one-time order can go
//! either way, selected by [`create_order`]'s `redeem` flag:
//!
//! - `redeem: true` — the server redeems the prepaid code into a key itself,
//!   so [`OrderResult::key`] is set and the buyer's email holds the **key**.
//!   This is what the app wants by default: a buyer who is emailed a one-time
//!   *code* that the app already spent behind their back reasonably mistakes
//!   that code for their credential.
//! - `redeem: false` — the classic flow: [`OrderResult::code`] is set and the
//!   caller exchanges it via `POST /v3/redeem`. Required to stack the bought
//!   time onto a key the buyer already holds, because only `/v3/redeem` takes
//!   a `renew_key` target — the server-side redeem always mints a fresh key.
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

use super::api::{check, http_client, normalize_base};

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
/// `status`: `pending` (keep polling) / `ready` (`plan`, `days` and exactly
/// one of `key` / `code` set) / `delivered` (retrieval window passed — the
/// credential was emailed) / `refunded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResult {
    pub status: String,
    /// The prepaid redeem code, present on `ready` for an order created with
    /// `redeem: false`. Feed it to `POST /v3/redeem` to turn it into a key.
    #[serde(default)]
    pub code: Option<String>,
    /// The API key itself, present on `ready` for an order created with
    /// `redeem: true` — the server already spent the code, so there is no
    /// redeem step and the code must never be re-redeemed. Branch on
    /// whichever of `key` / `code` is present rather than on the flag you
    /// sent, so an older server that ignores `redeem` still degrades to the
    /// classic flow.
    #[serde(default)]
    pub key: Option<String>,
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
struct CreateOrderRequest<'a> {
    product: &'a str,
    /// Redeem the code into a key server-side. Serialized as a real JSON
    /// bool on purpose — the server rejects `1` / `"true"` with a `400`.
    redeem: bool,
}

/// Subscriptions always hand back a key, so there is no `redeem` knob here.
#[derive(Serialize)]
struct CreateSubscriptionRequest<'a> {
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
///
/// `redeem` decides what the later [`order_result`] poll — and the buyer's
/// email — carries: `true` for the API key itself, `false` for a redeem code
/// the caller must exchange. Pass `false` only when the code is needed as a
/// code, i.e. to renew an existing key through `/v3/redeem`'s `renew_key`.
pub async fn create_order(
    base_url: &str,
    proxy: &str,
    product: &str,
    redeem: bool,
) -> Result<CreatedOrder> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/create-order");
    let resp = http_client(PURCHASE_TIMEOUT, proxy)?
        .post(&url)
        .json(&CreateOrderRequest {
            product: product.trim(),
            redeem,
        })
        .send()
        .await
        .context("POST /paypal/create-order")?;
    let resp = check(resp, "create order").await?;
    resp.json::<CreatedOrder>()
        .await
        .context("parse /paypal/create-order response")
}

/// `POST /paypal/order-result` (no auth) — poll a one-time purchase. On
/// `ready` the response carries the API key (`redeem: true` order) or the
/// redeem code (`redeem: false`); see [`OrderResult`].
///
/// Idempotent and safe to repeat every few seconds until a terminal status.
/// A wrong `claim` is a `404` and counts toward the per-IP failure guard, so
/// never retry with guessed secrets.
pub async fn order_result(
    base_url: &str,
    proxy: &str,
    order_id: &str,
    claim: &str,
) -> Result<OrderResult> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/order-result");
    let resp = http_client(PURCHASE_TIMEOUT, proxy)?
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
pub async fn create_subscription(
    base_url: &str,
    proxy: &str,
    product: &str,
) -> Result<CreatedSubscription> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/create-subscription");
    let resp = http_client(PURCHASE_TIMEOUT, proxy)?
        .post(&url)
        .json(&CreateSubscriptionRequest {
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
    proxy: &str,
    subscription_id: &str,
    claim: &str,
) -> Result<SubscriptionResult> {
    let base = normalize_base(base_url);
    let url = format!("{base}/paypal/subscription-result");
    let resp = http_client(PURCHASE_TIMEOUT, proxy)?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::test_http::mock_http;

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
        assert!(r.key.is_none());
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
        // The classic flow never pre-redeems; the caller must exchange it.
        assert!(r.key.is_none());
    }

    /// A `redeem: true` order resolves server-side: `ready` carries the key
    /// instead of the code. The caller must not look for `code` there — the
    /// code is already spent, and re-redeeming it would fail.
    #[test]
    fn order_result_ready_carries_key_when_server_redeemed() {
        let raw = r#"{"status":"ready","key":"k0000000000000000000000000000003",
                      "plan":"pro","days":30}"#;
        let r: OrderResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.status, "ready");
        assert_eq!(r.key.as_deref(), Some("k0000000000000000000000000000003"));
        assert!(r.code.is_none());
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
        let v = serde_json::to_value(CreateOrderRequest {
            product: "pro-30",
            redeem: true,
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({"product": "pro-30", "redeem": true}));
        // `redeem` must be a JSON bool — the server rejects `1` / `"true"`.
        assert!(v["redeem"].is_boolean());

        // Subscriptions always yield a key; they take no `redeem` knob, and
        // sending one would be a malformed body.
        let v = serde_json::to_value(CreateSubscriptionRequest {
            product: "pro-monthly",
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({"product": "pro-monthly"}));

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

    // ---- end-to-end against a local mock server (see `crate::bot::test_http`) ----

    /// `redeem: false` — the classic flow a renewal needs: `ready` hands back
    /// a code the caller exchanges itself (with a `renew_key` target).
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

        let created = create_order(&format!("{base}/"), "", "pro-30", false)
            .await
            .unwrap();
        assert_eq!(created.order_id, "OID1");
        assert_eq!(created.claim_secret, "S1");

        let pending = order_result(&base, "", &created.order_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(pending.status, "pending");

        let ready = order_result(&base, "", &created.order_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(ready.code.as_deref(), Some("code1234code5678"));
        assert!(ready.key.is_none());

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /paypal/create-order HTTP/1.1"));
        assert!(reqs[0].contains(r#"{"product":"pro-30","redeem":false}"#));
        assert!(reqs[1].starts_with("POST /paypal/order-result HTTP/1.1"));
        assert!(reqs[1].contains(r#""order_id":"OID1""#));
        assert!(reqs[1].contains(r#""claim":"S1""#));
    }

    /// Regression: `redeem: true` must reach the wire as a real JSON bool
    /// (the server 400s on `1` / `"true"`), and the resulting `ready` poll
    /// must surface the API key directly — no `/v3/redeem` step, so the
    /// buyer's emailed credential is the key they actually use.
    #[tokio::test]
    async fn redeem_true_order_roundtrip_returns_key_directly() {
        let (base, served) = mock_http(vec![
            (
                "200 OK",
                r#"{"order_id":"OID2","approve_url":"https://paypal.example/approve","claim_secret":"S3"}"#.into(),
            ),
            (
                "200 OK",
                r#"{"status":"ready","key":"k0000000000000000000000000000004","plan":"pro","days":30}"#.into(),
            ),
        ]);

        let created = create_order(&base, "", "pro-30", true).await.unwrap();
        assert_eq!(created.order_id, "OID2");

        let ready = order_result(&base, "", &created.order_id, &created.claim_secret)
            .await
            .unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(
            ready.key.as_deref(),
            Some("k0000000000000000000000000000004")
        );
        assert!(ready.code.is_none(), "a redeemed order exposes no code");

        let reqs = served.join().unwrap();
        assert!(reqs[0].starts_with("POST /paypal/create-order HTTP/1.1"));
        assert!(reqs[0].contains(r#"{"product":"pro-30","redeem":true}"#));
        assert!(
            !reqs[0].contains(r#""redeem":"true""#) && !reqs[0].contains(r#""redeem":1"#),
            "redeem must be a JSON bool, got: {}",
            reqs[0]
        );
    }

    /// `create-subscription` must never carry `redeem` — subscriptions always
    /// mint a key, and an unexpected field risks a `400 bad request`.
    #[tokio::test]
    async fn create_subscription_omits_the_redeem_flag() {
        let (base, served) = mock_http(vec![(
            "200 OK",
            r#"{"subscription_id":"I-2","approve_url":"https://paypal.example/sub","claim_secret":"S4"}"#.into(),
        )]);

        create_subscription(&base, "", "pro-monthly").await.unwrap();

        let reqs = served.join().unwrap();
        assert!(reqs[0].contains(r#"{"product":"pro-monthly"}"#));
        assert!(!reqs[0].contains("redeem"), "got: {}", reqs[0]);
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

        let created = create_subscription(&base, "", "pro-monthly").await.unwrap();
        assert_eq!(created.subscription_id, "I-1");

        let ready = subscription_result(&base, "", &created.subscription_id, &created.claim_secret)
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
        let err = create_order(&base, "", "nope-99", true).await.unwrap_err();
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
        let err = order_result(&base, "", "OID1", "WRONG").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 404"), "got: {msg}");
        served.join().unwrap();
    }
}
