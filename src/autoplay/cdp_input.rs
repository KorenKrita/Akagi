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

  // Majsoul's request/response envelope starts with a type byte followed by
  // a little-endian u16 msg_id.  Autoplay sends through the very same wrapper
  // as the game client so one monotonically increasing sequence owns the wire
  // for each WebSocket, regardless of who produced a request.
  const LIQI_REQUEST = 2;
  const LIQI_RESPONSE = 3;
  function nextMsgId(id) {
    return id === 0xffff ? 1 : id + 1;
  }
  function bytesOf(data) {
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (ArrayBuffer.isView(data)) {
      return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    }
    return null;
  }
  function copyWithMsgId(data, msgId) {
    const source = bytesOf(data);
    if (!source || source.length < 3) return data;
    const copy = new Uint8Array(source);
    copy[1] = msgId & 0xff;
    copy[2] = msgId >>> 8;
    if (data instanceof ArrayBuffer) return copy.buffer;
    if (data instanceof DataView) return new DataView(copy.buffer);
    if (ArrayBuffer.isView(data) && data.constructor !== Uint8Array) {
      try { return new data.constructor(copy.buffer); } catch (_) {}
    }
    return copy;
  }
  function msgIdOf(data, type) {
    const bytes = bytesOf(data);
    if (!bytes || bytes.length < 3 || bytes[0] !== type) return null;
    return bytes[1] | (bytes[2] << 8);
  }

  function remember(ws, url) {
    const resolvedUrl = String(url || ws.url || "");
    const nativeSend = ws.send.bind(ws);
    const state = {
      nextMsgId: null,
      // Wire id -> id supplied by the game/autoplay producer. Responses are
      // translated through this table before page code can observe them.
      pendingByWireId: new Map(),
      // Retained for diagnostics: every non-sequential client id records both
      // the supplied value and the value placed on the wire.
      corrections: [],
    };
    const entry = { ws, url: resolvedUrl, msgIds: state };
    window.__akagiSockets.push(entry);

    function prepareRequest(data) {
      const originalId = msgIdOf(data, LIQI_REQUEST);
      if (originalId === null) return data;

      // The first uplink request establishes this connection's baseline. All
      // later requests, including injected autoplay requests, consume exactly
      // one id from this per-socket sequence.
      const wireId = state.nextMsgId === null ? originalId : state.nextMsgId;
      if (state.nextMsgId !== null && originalId !== wireId) {
        const correction = { original: originalId, expected: wireId };
        state.corrections.push(correction);
        if (state.corrections.length > 256) state.corrections.shift();
        console.warn(
          `[Akagi] corrected WebSocket msg_id ${originalId} -> ${wireId}`,
          resolvedUrl
        );
      }
      state.pendingByWireId.set(wireId, originalId);
      state.nextMsgId = nextMsgId(wireId);
      return originalId === wireId ? data : copyWithMsgId(data, wireId);
    }

    // Blob contents are only readable asynchronously. Once one is queued,
    // queue subsequent sends behind it as well so their wire order and ids
    // still match the client's call order.
    let sendQueue = Promise.resolve();
    let queuedSends = 0;
    function enqueueSend(task) {
      queuedSends += 1;
      sendQueue = sendQueue
        .then(task)
        .catch((error) => console.error('[Akagi] WebSocket send translation failed', error))
        .finally(() => { queuedSends -= 1; });
    }
    ws.send = function(data) {
      if (data instanceof Blob) {
        enqueueSend(async () => {
          const buffer = await data.arrayBuffer();
          const prepared = prepareRequest(buffer);
          const outgoing = prepared === buffer
            ? data
            : new Blob([prepared], { type: data.type });
          nativeSend(outgoing);
        });
        return;
      }
      if (queuedSends > 0) {
        enqueueSend(() => nativeSend(prepareRequest(data)));
        return;
      }
      return nativeSend(prepareRequest(data));
    };

    // This listener is installed inside the constructor, before the game can
    // register its own listeners. A translated synthetic event is dispatched
    // synchronously, preserving listener order and the WebSocket event API.
    const translatedEvents = new WeakSet();
    let blobMessages = Promise.resolve();
    ws.addEventListener('message', (event) => {
      if (translatedEvents.has(event)) return;

      // `binaryType` defaults to "blob". Queue all Blob events (not only
      // responses) so the asynchronous conversion cannot reorder a notify
      // relative to the response before or after it.
      if (event.data instanceof Blob) {
        event.stopImmediatePropagation();
        blobMessages = blobMessages.then(async () => {
          const buffer = await event.data.arrayBuffer();
          const wireId = msgIdOf(buffer, LIQI_RESPONSE);
          let data = event.data;
          if (wireId !== null && state.pendingByWireId.has(wireId)) {
            const originalId = state.pendingByWireId.get(wireId);
            state.pendingByWireId.delete(wireId);
            if (originalId !== wireId) {
              data = new Blob([copyWithMsgId(buffer, originalId)], { type: event.data.type });
            }
          }
          const translated = new MessageEvent('message', {
            data,
            origin: event.origin,
            lastEventId: event.lastEventId,
          });
          translatedEvents.add(translated);
          ws.dispatchEvent(translated);
        }).catch((error) => console.error('[Akagi] WebSocket message translation failed', error));
        return;
      }

      const wireId = msgIdOf(event.data, LIQI_RESPONSE);
      if (wireId === null || !state.pendingByWireId.has(wireId)) return;
      const originalId = state.pendingByWireId.get(wireId);
      state.pendingByWireId.delete(wireId);
      if (originalId === wireId) return;

      event.stopImmediatePropagation();
      const translated = new MessageEvent('message', {
        data: copyWithMsgId(event.data, originalId),
        origin: event.origin,
        lastEventId: event.lastEventId,
      });
      translatedEvents.add(translated);
      ws.dispatchEvent(translated);
    });

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
    dispatch_click_shaped(page, x, y, hover_delay_ms, click_hold_ms, false).await
}

/// As [`dispatch_click`], but able to vary the *shape* of the press.
///
/// `jiggle` nudges the cursor a pixel mid-press and puts it back. It
/// exists for retries: when a press lands on the right control and the
/// action still does not commit, the position is not what is wrong, so
/// the only thing left to change is how the press is made.
pub async fn dispatch_click_shaped(
    page: &Page,
    x: f64,
    y: f64,
    hover_delay_ms: u32,
    click_hold_ms: u32,
    jiggle: bool,
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

    if jiggle {
        // Split the hold around the nudge so its total stays what the
        // config says. The move is one CSS pixel — enough for the engine
        // to resample the cursor while the button is down, not enough to
        // leave the control.
        let half = u64::from(click_hold_ms) / 2;
        if half > 0 {
            tokio::time::sleep(Duration::from_millis(half)).await;
        }
        page.move_mouse(Point::new(x + 1.0, y))
            .await
            .context("CDP move_mouse (jiggle out)")?;
        page.move_mouse(pt)
            .await
            .context("CDP move_mouse (jiggle back)")?;
        let rest = u64::from(click_hold_ms) - half;
        if rest > 0 {
            tokio::time::sleep(Duration::from_millis(rest)).await;
        }
    } else if click_hold_ms > 0 {
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

pub async fn dispatch_mouse_wheel(page: &Page, x: f64, y: f64, delta_y: f64) -> Result<()> {
    let wheel = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseWheel)
        .x(x)
        .y(y)
        .delta_x(0.0)
        .delta_y(delta_y)
        .build()
        .map_err(|e| anyhow!("build mouseWheel: {e}"))?;
    page.execute(wheel).await.context("CDP mouseWheel")?;
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

// ============================================================================
// Tenhou actuation
// ============================================================================
//
// Tenhou's client owns its board state: when you discard, its own handler
// updates the board *and* sends the frame, and its receive path then
// deliberately ignores the server's echo of that discard
// (`1==U.a && "D"==c.tag || Nb.cb(c)`) because it has already applied it.
// Writing the frame onto the socket behind the client's back therefore
// freezes the board — the local apply never happened and the echo is
// skipped. Everything here exists to drive the client's *own* input path
// instead, so its state machine stays in step.
//
// Two routes, because the client has two:
//
// - Buttons (chi/pon/kan/riichi/ron/tsumo/kyuushu/kita/pass) are real DOM
//   elements carrying `class="s7" name="c22-<slot>"`, routed by a
//   body-level click listener into the client's own handler. A dispatched
//   click is indistinguishable from the user's.
// - The discard is a canvas hit-test with no DOM element, so it needs a
//   pixel position — see [`probe_hand_geometry`].

/// Slots in the client's action menu. The client builds this menu itself
/// from the server's `t` bitmask, and the slot number *is* the meaning.
pub mod menu {
    pub const TSUMO_AGARI: u8 = 0;
    pub const RON: u8 = 1;
    pub const RIICHI: u8 = 2;
    pub const KYUUSHU: u8 = 3;
    pub const PASS: u8 = 4;
    /// 5..=9 — kita (sanma), one per distinct North the client offers.
    pub const KITA_FIRST: u8 = 5;
    /// 10..=12 — ankan / kakan candidates.
    pub const KAN_FIRST: u8 = 10;
    pub const DAIMINKAN: u8 = 13;
    /// The pon, spending the red five if the hand holds one. Always drawn
    /// when a pon is on offer — the client's builder writes it for all three
    /// shapes of holding (two plain copies, red + plain, or the red of a
    /// three-copy set).
    pub const PON: u8 = 15;
    /// The pon that *keeps* the red five out of the meld, drawn only when
    /// there is a choice to make: exactly one red copy and two plain ones.
    /// It holds the two plain copies.
    pub const PON_KEEP_RED: u8 = 14;
    /// 16..=21 — chi, in pairs running called-tile-lowest, -middle,
    /// -highest. The odd slot of each pair spends no red five; the even one
    /// below it does. Unlike pon, either can be drawn without the other:
    /// which exist is decided per shape by which copies the hand holds.
    pub const CHI_FIRST: u8 = 16;
}

/// CSS selector for one action button.
pub fn action_button_selector(slot: u8) -> String {
    format!(r#"button.s7[name="c22-{slot}"]"#)
}

/// Which action buttons the client is currently showing, in document order.
///
/// The client rebuilds this set on every decision window, so it is the
/// authoritative list of what may be pressed right now — better than
/// re-deriving the menu from the `t` bitmask and hoping the two agree.
///
/// Left in the client's own order rather than sorted: it renders highest slot
/// first, and seeing that in a log is what tells you a selector list was
/// resolved the wrong way round.
pub async fn list_action_buttons(page: &Page) -> Result<Vec<u8>> {
    let expr = "(()=>Array.from(document.querySelectorAll('button.s7[name^=\"c22-\"]'))\
                .map(b=>parseInt(b.getAttribute('name').slice(4),10))\
                .filter(n=>!isNaN(n)))()";
    let result = page
        .evaluate(expr)
        .await
        .context("CDP evaluate action button list")?;
    let value = result
        .value()
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![]));
    Ok(serde_json::from_value::<Vec<u8>>(value).unwrap_or_default())
}

/// Dispatch a real click on the first of `selectors` the page actually has.
///
/// The list is a *preference* order, and it has to be honoured as one: the
/// client appends its buttons in **descending** slot number
/// (`Object.keys(k).sort((a,n)=>n-a)`), so a single comma-joined selector
/// would resolve through `querySelector`'s document order and hand back the
/// highest-numbered match — the opposite of what a caller listing
/// "this one, or that one" means.
///
/// Returns `Ok(false)` when none match — the window closed while we were
/// thinking, or the client never offered any of them. Callers report that as
/// a skipped action rather than pressing something else.
pub async fn click_dom(page: &Page, selectors: &[String]) -> Result<bool> {
    let literal = serde_json::to_string(selectors).context("encode selectors as JS literal")?;
    let expr = format!(
        "(()=>{{for(const s of {literal}){{const e=document.querySelector(s);\
          if(e){{e.click();return true;}}}}return false;}})()"
    );
    let result = page
        .evaluate(expr)
        .await
        .context("CDP evaluate DOM click")?;
    Ok(result.value().and_then(|v| v.as_bool()).unwrap_or(false))
}

/// Is the client taking input yet?
///
/// The client's turn clock and its highlight box are raised by the same call
/// (`Ub.O`), at the end of the animation for whatever opened the window — so
/// the box appearing *is* "animation finished, clock started". Frame arrival
/// is not: the server can deliver several seats' actions at once and the
/// client will spend seconds animating them.
pub async fn turn_clock_running(page: &Page) -> Result<bool> {
    let expr = "(()=>{for(const d of document.querySelectorAll('div')){\
        const s=d.style; if(s.position!=='fixed'||s.display==='none')continue;\
        const c=d.firstElementChild;\
        if(!c||!c.classList||!c.classList.contains('ts2'))continue;\
        const r=d.getBoundingClientRect();\
        if(r.width>0&&r.height>0)return true;}\
      return false;})()";
    let result = page
        .evaluate(expr)
        .await
        .context("CDP evaluate turn clock probe")?;
    Ok(result.value().and_then(|v| v.as_bool()).unwrap_or(false))
}

/// Discard `tile_index` through the client's own handler.
///
/// `Ok(false)` means the client script was never instrumented, so the handler
/// is not reachable — reported as a skipped action rather than fumbled into a
/// click at a guessed position.
pub async fn discard_tile(page: &Page, tile_index: u32) -> Result<bool> {
    let expr = crate::autoplay::tenhou::inject::discard_expression(tile_index);
    let result = page.evaluate(expr).await.context("CDP evaluate discard")?;
    Ok(result.value().and_then(|v| v.as_str()) == Some("ok"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_names_the_menu_slot() {
        assert_eq!(
            action_button_selector(menu::PASS),
            r#"button.s7[name="c22-4"]"#
        );
        assert_eq!(
            action_button_selector(menu::RIICHI),
            r#"button.s7[name="c22-2"]"#
        );
    }

    /// A selector containing quotes must not break out of the JS literal.
    #[test]
    fn selector_is_escaped_into_the_expression() {
        let literal = serde_json::to_string(r#"button.s7[name="c22-4"]"#).unwrap();
        assert!(literal.contains(r#"\"c22-4\""#));
    }
}
