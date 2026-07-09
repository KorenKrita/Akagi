//! Thin wrappers around chromiumoxide for the autoplay manager.
//!
//! Centralised so the click + canvas-rect query logic can be unit-mocked
//! and the manager keeps a single dependency on chromiumoxide types.

use crate::autoplay::context::CanvasRect;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use chromiumoxide::layout::Point;
use chromiumoxide::page::Page;
use std::time::Duration;

const WS_HOOK_SCRIPT: &str = r#"
(() => {
  const NativeWebSocket = window.__akagiNativeWebSocket || window.WebSocket;
  window.__akagiNativeWebSocket = NativeWebSocket;
  window.__akagiSockets = window.__akagiSockets || [];
  function remember(ws, url) {
    const resolvedUrl = String(url || ws.url || "");
    window.__akagiSockets.push({ ws, url: resolvedUrl });
    ws.addEventListener('close', () => {
      window.__akagiSockets = (window.__akagiSockets || []).filter((s) => (s.ws || s) !== ws);
    });
  }
  if (!window.__akagiWsHookInstalled) {
    window.__akagiWsHookInstalled = true;
    function AkagiWebSocket(...args) {
      const ws = new NativeWebSocket(...args);
      remember(ws, args[0]);
      return ws;
    }
    AkagiWebSocket.prototype = NativeWebSocket.prototype;
    Object.setPrototypeOf(AkagiWebSocket, NativeWebSocket);
    for (const key of Object.getOwnPropertyNames(NativeWebSocket)) {
      if (!(key in AkagiWebSocket)) {
        try { Object.defineProperty(AkagiWebSocket, key, Object.getOwnPropertyDescriptor(NativeWebSocket, key)); } catch (_) {}
      }
    }
    window.WebSocket = AkagiWebSocket;
  }
  function urlsLikelySame(a, b) {
    if (!a || !b) return false;
    if (a === b || a.includes(b) || b.includes(a)) return true;
    try {
      const au = new URL(a);
      const bu = new URL(b);
      return au.host === bu.host && au.pathname === bu.pathname;
    } catch (_) {
      return false;
    }
  }
  window.__akagiSendWsBase64 = (b64, targetUrl) => {
    const sockets = (window.__akagiSockets || [])
      .map((s) => ({ ws: s.ws || s, url: String(s.url || (s.ws || s).url || "") }))
      .filter((s) => s.ws && s.ws.readyState === NativeWebSocket.OPEN);
    let target = sockets.find((s) => urlsLikelySame(s.url, targetUrl));
    if (!target && sockets.length === 1) target = sockets[0];
    if (!target) return false;
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    target.ws.send(bytes);
    return true;
  };
})();
"#;

pub async fn install_ws_hook(page: &Page) -> Result<()> {
    page.evaluate_on_new_document(WS_HOOK_SCRIPT)
        .await
        .context("CDP install WebSocket init hook")?;
    page.evaluate(WS_HOOK_SCRIPT)
        .await
        .context("CDP install WebSocket live hook")?;
    Ok(())
}

pub async fn dispatch_ws_binary(page: &Page, target_url: &str, bytes: &[u8]) -> Result<bool> {
    let b64 = BASE64.encode(bytes);
    let expr = format!(
        "(()=>window.__akagiSendWsBase64 && window.__akagiSendWsBase64({}, {}))()",
        serde_json::to_string(&b64).expect("base64 string serializes"),
        serde_json::to_string(target_url).expect("target url string serializes")
    );
    let result = page
        .evaluate(expr)
        .await
        .context("CDP send WebSocket frame")?;
    Ok(result.value().and_then(|v| v.as_bool()).unwrap_or(false))
}

/// Dispatch a mouse move without pressing. Used to force a fresh hover
/// transition before retrying a click that may have been swallowed.
pub async fn dispatch_mouse_move(page: &Page, x: f64, y: f64) -> Result<()> {
    page.move_mouse(Point::new(x, y))
        .await
        .context("CDP move_mouse")?;
    Ok(())
}

/// Dispatch a single mouse click at `(x, y)` (CSS pixels) as four CDP
/// events, with mandatory hover before press:
///
/// 1. `mouseMoved` to `(x, y)`
/// 2. sleep `hover_delay_ms` (≥100ms — Laya's input system samples hover
///    state before mousedown registers a hit on a tile sprite)
/// 3. `mousePressed`
/// 4. sleep `click_hold_ms`
/// 5. `mouseReleased`
///
/// `chromiumoxide::Page::click` collapses 3+5 into back-to-back frames
/// without the hover delay, which Majsoul drops on the floor for hand
/// tiles. Hand-rolling the sequence is required.
pub async fn dispatch_click(
    page: &Page,
    x: f64,
    y: f64,
    hover_delay_ms: u32,
    click_hold_ms: u32,
) -> Result<()> {
    let pt = Point::new(x, y);
    dispatch_mouse_move(page, x, y).await?;
    if hover_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(hover_delay_ms as u64)).await;
    }

    let press = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MousePressed)
        .x(pt.x)
        .y(pt.y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()
        .map_err(|e| anyhow!("build mousePressed: {e}"))?;
    page.execute(press).await.context("CDP mousePressed")?;

    if click_hold_ms > 0 {
        tokio::time::sleep(Duration::from_millis(click_hold_ms as u64)).await;
    }

    let release = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseReleased)
        .x(pt.x)
        .y(pt.y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()
        .map_err(|e| anyhow!("build mouseReleased: {e}"))?;
    page.execute(release).await.context("CDP mouseReleased")?;

    Ok(())
}

/// Read the game canvas's `getBoundingClientRect()` via `Runtime.evaluate`.
///
/// Majsoul renders into the first `<canvas>` element on the page; Tenhou
/// uses the same selector when running in browser mode. We grab the
/// first canvas indiscriminately — multi-canvas pages aren't a thing on
/// these platforms.
pub async fn evaluate_canvas_rect(page: &Page) -> Result<CanvasRect> {
    // IIFE so `Runtime.evaluate` returns a single value, not a Promise.
    // `is_likely_js_function` in chromiumoxide picks the right CDP call
    // based on whether the expression looks like a function — we wrap
    // in `(()=>{...})()` to ensure plain-expression evaluation.
    let expr = "(()=>{const c=document.getElementsByTagName('canvas')[0];\
                if(!c)return null;\
                const r=c.getBoundingClientRect();\
                return {x:r.x,y:r.y,width:r.width,height:r.height};})()";
    let result = page
        .evaluate(expr)
        .await
        .context("CDP evaluate canvas rect")?;
    let value = result
        .value()
        .ok_or_else(|| anyhow!("canvas rect: no value returned"))?;
    if value.is_null() {
        return Err(anyhow!("canvas rect: page has no <canvas> element"));
    }
    let rect: CanvasRect = serde_json::from_value(value.clone())
        .context("canvas rect: deserialise from page value")?;
    Ok(rect)
}
