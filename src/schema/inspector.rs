//! Inspector pipeline records — the unified data model for the
//! Logs → Inspector tab.
//!
//! One canonical struct per pipeline stage:
//!
//! - `WsFrame` — raw bytes the proxy or chromium capture saw, with the
//!   bridge's first-pass parsed view alongside.
//! - `MjaiEvent` — translated game event from `mjai_bus`.
//! - `BotReaction` — bot's response with the triggering mjai event and
//!   reaction latency, so "why did the bot do that?" is answerable from a
//!   single record.
//!
//! Same shape on the wire (live tail over `tauri::ipc::Channel`) and on
//! disk (`<session>/inspector.jsonl`). The on-disk file is the source of
//! truth for past-session viewing; the bus/channel is the live tail.

use super::MjaiEvent;
use serde::{Deserialize, Serialize};

/// Direction of a captured WS frame relative to the proxied client.
/// Self-contained so the schema crate doesn't depend on `crate::bridge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameDirection {
    Up,
    Down,
}

/// Raw wire bytes of a WS frame, in the form Chrome / hudsucker delivered.
///
/// `Text` carries opcode-1 frames as their literal UTF-8 string (Tenhou's
/// `{tag:…}` JSON, the `<Z/>` heartbeat). `Binary` carries opcode-2 frames
/// as base64 — the JSONL line stays printable, the frontend can hex-render
/// it on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", content = "data", rename_all = "lowercase")]
pub enum FrameRaw {
    Text(String),
    Binary(String),
}

/// Bridge's structured view of a parsed frame.
///
/// `method` is the platform-native message identifier (Majsoul method
/// name like `.lq.ActionPrototype`, Tenhou tag like `INIT`/`T0`/`AGARI`),
/// `args` is whatever the bridge already produced internally — protobuf
/// decoded to JSON for Majsoul, the JSON dict for Tenhou. Bridges that
/// can't decode a particular frame (handshake, unsupported method) return
/// `None`; the inspector then only shows raw bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedFrame {
    pub method: String,
    pub args: serde_json::Value,
}

/// Bot reaction record. Captures the triggering mjai event AND the bot's
/// response in one payload so the user can debug "why did the bot do
/// that?" without cross-referencing two files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotReaction {
    pub bot: String,
    pub actor_id: u8,
    pub trigger: MjaiEvent,
    pub action: MjaiEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
    /// React-call latency in milliseconds.
    pub reaction_ms: u64,
}

/// One row in the inspector timeline.
///
/// Tagged on `kind` so the React side can switch on a string discriminant
/// without ever having to know the field shape of the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectorEntry {
    /// WebSocket frame seen by the capture backend.
    WsFrame {
        ts_ms: i64,
        direction: FrameDirection,
        /// Bridge instance identifier — one per WS connection. Lets the
        /// frontend group frames by flow when multiple flows are live.
        flow_id: String,
        size: usize,
        raw: FrameRaw,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsed: Option<ParsedFrame>,
        /// Number of mjai events the bridge emitted from this frame —
        /// surfaced in the row so the user spots "frame parsed but
        /// produced 0 events" at a glance.
        emitted: usize,
    },
    /// MJAI event observed on `mjai_bus`.
    MjaiEvent { ts_ms: i64, event: MjaiEvent },
    /// Bot reaction captured at the bot manager's response site.
    BotReaction {
        ts_ms: i64,
        #[serde(flatten)]
        reaction: BotReaction,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_frame_text_round_trips() {
        let entry = InspectorEntry::WsFrame {
            ts_ms: 1_700_000_000_000,
            direction: FrameDirection::Down,
            flow_id: "tenhou:000001".into(),
            size: 38,
            raw: FrameRaw::Text(r#"{"tag":"INIT","seed":"1,0,0,2,5,134"}"#.into()),
            parsed: Some(ParsedFrame {
                method: "INIT".into(),
                args: serde_json::json!({"seed":"1,0,0,2,5,134"}),
            }),
            emitted: 1,
        };
        let j = serde_json::to_string(&entry).unwrap();
        assert!(j.contains(r#""kind":"ws_frame""#));
        assert!(j.contains(r#""direction":"down""#));
        assert!(j.contains(r#""format":"text""#));
        let back: InspectorEntry = serde_json::from_str(&j).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn ws_frame_binary_round_trips() {
        let entry = InspectorEntry::WsFrame {
            ts_ms: 1,
            direction: FrameDirection::Up,
            flow_id: "majsoul:000001".into(),
            size: 5,
            raw: FrameRaw::Binary("AAECA2g=".into()),
            parsed: None,
            emitted: 0,
        };
        let j = serde_json::to_string(&entry).unwrap();
        assert!(j.contains(r#""format":"binary""#));
        // `parsed: None` is skipped, not emitted as `null`.
        assert!(!j.contains(r#""parsed""#));
        let back: InspectorEntry = serde_json::from_str(&j).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn mjai_event_round_trips() {
        let entry = InspectorEntry::MjaiEvent {
            ts_ms: 5,
            event: MjaiEvent::Tsumo {
                actor: 0,
                pai: "5m".into(),
            },
        };
        let j = serde_json::to_string(&entry).unwrap();
        assert!(j.contains(r#""kind":"mjai_event""#));
        assert!(j.contains(r#""type":"tsumo""#));
        let back: InspectorEntry = serde_json::from_str(&j).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn bot_reaction_round_trips() {
        let entry = InspectorEntry::BotReaction {
            ts_ms: 7,
            reaction: BotReaction {
                bot: "mortal".into(),
                actor_id: 2,
                trigger: MjaiEvent::Tsumo {
                    actor: 2,
                    pai: "5m".into(),
                },
                action: MjaiEvent::Dahai {
                    actor: 2,
                    pai: "W".into(),
                    tsumogiri: false,
                },
                meta: Some(serde_json::json!({"q": [0.1, 0.2]})),
                reaction_ms: 44,
            },
        };
        let j = serde_json::to_string(&entry).unwrap();
        assert!(j.contains(r#""kind":"bot_reaction""#));
        assert!(j.contains(r#""bot":"mortal""#));
        assert!(j.contains(r#""reaction_ms":44"#));
        let back: InspectorEntry = serde_json::from_str(&j).unwrap();
        assert_eq!(back, entry);
    }
}
