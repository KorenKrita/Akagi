//! Shared state between the chromium capture backend and the autoplay
//! manager.
//!
//! - `page`: the [`chromiumoxide::page::Page`] handle for the tab where
//!   Majsoul (or another supported platform) is loaded. Written by
//!   `src/capture/chromium/cdp.rs` when it observes a WebSocket whose URL
//!   host matches a known platform. The handle tracks the **tab**, not the
//!   WebSocket: it survives the many short-lived Route-probe / lobby-
//!   reconnect sockets Majsoul opens and closes during a game, and is
//!   cleared only when its owning tab is removed from the page snapshot.
//!   Read by `AutoplayManager` whenever it needs to dispatch input.
//! - `canvas_rect`: cached `getBoundingClientRect()` of the game canvas,
//!   used to translate 16:9-normalised coordinates into CSS pixels.
//!   Filled lazily by the autoplay manager (one `Runtime.evaluate` per
//!   refresh) and invalidated on round transitions.
//!
//! The page/canvas fields are populated only by Chromium. Packet dispatch
//! fields can instead be populated by the MITM backend.

use crate::capture::flow::SharedBridge;
use chromiumoxide::page::Page;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock};

#[derive(Default)]
pub struct AutoplayContext {
    pub page: Arc<RwLock<Option<Page>>>,
    pub canvas_rect: Arc<RwLock<Option<CanvasRect>>>,
    pub packet_bridge: Arc<RwLock<Option<SharedBridge>>>,
    pub packet_ws_url: Arc<RwLock<Option<String>>>,
    /// MITM client→server WebSocket leg selected from the gameplay flow.
    pub mitm_packet_tx: Arc<RwLock<Option<mpsc::UnboundedSender<Vec<u8>>>>>,
    /// The most recently connected gameplay flow, used as the fallback when
    /// no ActionPrototype flow has been observed yet.
    pub fallback_page: Arc<RwLock<Option<Page>>>,
    pub fallback_packet_bridge: Arc<RwLock<Option<SharedBridge>>>,
    pub fallback_packet_ws_url: Arc<RwLock<Option<String>>>,
    /// Flow id that most recently delivered a gameplay ActionPrototype.
    pub preferred_packet_flow: Arc<RwLock<Option<String>>>,
    injected_ws_frames: Arc<Mutex<VecDeque<u64>>>,
    /// Server-granted time budget for the current decision window.
    /// Written by the Majsoul bridge (see `autoplay::budget`), read by
    /// the manager's delay model. Uses a `std::sync::RwLock` (not tokio)
    /// because the writer is the bridge's synchronous `parse()` path.
    pub time_budget: crate::autoplay::budget::SharedTimeBudget,
    /// Counter of accepted Mahjong Soul uplink input commands.
    pub input_watch: crate::autoplay::verify::SharedInputWatch,
    /// Tenhou's hand at tile-index resolution plus its current decision
    /// window, written by the Tenhou bridge (see `autoplay::tenhou_state`).
    /// Read by the Tenhou autoplay planner, which encodes a client frame
    /// rather than synthesising clicks.
    pub tenhou_state: crate::autoplay::tenhou_state::SharedTenhouState,
    /// Frame injection channel for platforms whose client is not a browser
    /// page (Riichi City): the manager sends built wire frames, the MITM
    /// proxy's client→server relay transmits them. The `in_game` gate is
    /// maintained by the Riichi City bridge. See `autoplay::inject`.
    pub inject: crate::autoplay::inject::SharedInjectBus,
    auto_join: StdMutex<AutoJoinRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoJoinPhase {
    Disabled,
    WaitingForLobby,
    Settling,
    Joining,
    Matching,
    InGame,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoJoinStopReason {
    GameLimit,
    TimeLimit,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoJoinStatus {
    pub enabled: bool,
    pub running: bool,
    pub phase: AutoJoinPhase,
    pub stop_reason: Option<AutoJoinStopReason>,
    pub completed_games: u32,
    pub max_games: Option<u32>,
    pub remaining_games: Option<u32>,
    pub remaining_seconds: Option<u64>,
}

#[derive(Debug, Default)]
struct AutoJoinRuntime {
    started_at: Option<Instant>,
    completed_games: u32,
    phase: Option<AutoJoinPhase>,
}

impl AutoplayContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn mark_injected_ws_frame(&self, bytes: &[u8]) {
        let mut frames = self.injected_ws_frames.lock().await;
        frames.push_back(frame_fingerprint(bytes));
        while frames.len() > 32 {
            frames.pop_front();
        }
    }

    pub async fn take_injected_ws_frame_mark(&self, bytes: &[u8]) -> bool {
        let fingerprint = frame_fingerprint(bytes);
        let mut frames = self.injected_ws_frames.lock().await;
        if let Some(pos) = frames.iter().position(|f| *f == fingerprint) {
            frames.remove(pos);
            true
        } else {
            false
        }
    }

    pub async fn send_mitm_packet(
        &self,
        frame: &[u8],
    ) -> Option<Result<(), mpsc::error::SendError<Vec<u8>>>> {
        self.mitm_packet_tx
            .read()
            .await
            .clone()
            .map(|tx| tx.send(frame.to_vec()))
    }

    pub fn auto_join_start(&self) {
        let mut state = self.auto_join.lock().expect("auto-join mutex poisoned");
        state.started_at.get_or_insert_with(Instant::now);
        state.phase = Some(AutoJoinPhase::WaitingForLobby);
    }

    pub fn auto_join_set_phase(&self, phase: AutoJoinPhase) {
        let mut state = self.auto_join.lock().expect("auto-join mutex poisoned");
        state.phase = Some(phase);
    }

    pub fn auto_join_record_completed(&self) {
        let mut state = self.auto_join.lock().expect("auto-join mutex poisoned");
        state.started_at.get_or_insert_with(Instant::now);
        state.completed_games = state.completed_games.saturating_add(1);
        state.phase = Some(AutoJoinPhase::Settling);
    }

    pub fn auto_join_can_join(&self, cfg: &crate::config::MajsoulAutoplayConfig) -> bool {
        let mut state = self.auto_join.lock().expect("auto-join mutex poisoned");
        state.started_at.get_or_insert_with(Instant::now);
        let reason = auto_join_stop_reason(&state, cfg);
        state.phase = Some(if reason.is_some() {
            AutoJoinPhase::Stopped
        } else {
            AutoJoinPhase::Joining
        });
        reason.is_none()
    }

    pub fn auto_join_status(
        &self,
        enabled: bool,
        cfg: &crate::config::MajsoulAutoplayConfig,
    ) -> AutoJoinStatus {
        let state = self.auto_join.lock().expect("auto-join mutex poisoned");
        let enabled = enabled && cfg.auto_join_game;
        let reason = enabled
            .then(|| auto_join_stop_reason(&state, cfg))
            .flatten();
        let max_games =
            (cfg.auto_join_stop_after_games > 0).then_some(cfg.auto_join_stop_after_games);
        let remaining_games = max_games.map(|max| max.saturating_sub(state.completed_games));
        let remaining_seconds = (cfg.auto_join_stop_after_minutes > 0).then(|| {
            let limit = Duration::from_secs(u64::from(cfg.auto_join_stop_after_minutes) * 60);
            let elapsed = state.started_at.map(|at| at.elapsed()).unwrap_or_default();
            limit.saturating_sub(elapsed).as_secs()
        });
        AutoJoinStatus {
            enabled,
            running: enabled && reason.is_none(),
            phase: if !enabled {
                AutoJoinPhase::Disabled
            } else if reason.is_some() {
                AutoJoinPhase::Stopped
            } else {
                state.phase.unwrap_or(AutoJoinPhase::WaitingForLobby)
            },
            stop_reason: reason,
            completed_games: state.completed_games,
            max_games,
            remaining_games,
            remaining_seconds,
        }
    }
}

fn auto_join_stop_reason(
    state: &AutoJoinRuntime,
    cfg: &crate::config::MajsoulAutoplayConfig,
) -> Option<AutoJoinStopReason> {
    if cfg.auto_join_stop_after_games > 0 && state.completed_games >= cfg.auto_join_stop_after_games
    {
        return Some(AutoJoinStopReason::GameLimit);
    }
    if cfg.auto_join_stop_after_minutes > 0
        && state.started_at.is_some_and(|at| {
            at.elapsed() >= Duration::from_secs(u64::from(cfg.auto_join_stop_after_minutes) * 60)
        })
    {
        return Some(AutoJoinStopReason::TimeLimit);
    }
    None
}

fn frame_fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// CSS-pixel bounding rect for the game canvas, as reported by
/// `Element.getBoundingClientRect()`. `(x, y)` is the top-left of the
/// canvas relative to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CanvasRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CanvasRect {
    /// Translate a 16:9 normalised point (the coordinate system used by
    /// `LOCATION` tables ported from the Python reference) to CSS pixels.
    pub fn pixel(&self, x_norm: f64, y_norm: f64) -> (f64, f64) {
        (
            self.x + (x_norm / 16.0) * self.width,
            self.y + (y_norm / 9.0) * self.height,
        )
    }

    /// Sanity check for a normalised point — clamps off-canvas requests
    /// before we hand them to CDP.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_translation_centre() {
        let rect = CanvasRect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: 900.0,
        };
        assert_eq!(rect.pixel(8.0, 4.5), (800.0, 450.0));
    }

    #[test]
    fn pixel_translation_with_offset() {
        let rect = CanvasRect {
            x: 100.0,
            y: 50.0,
            width: 1280.0,
            height: 720.0,
        };
        let (px, py) = rect.pixel(8.0, 4.5);
        assert!((px - (100.0 + 640.0)).abs() < 1e-9);
        assert!((py - (50.0 + 360.0)).abs() < 1e-9);
    }

    #[test]
    fn contains_inside() {
        let rect = CanvasRect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: 900.0,
        };
        assert!(rect.contains(800.0, 450.0));
    }

    #[test]
    fn contains_outside() {
        let rect = CanvasRect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: 900.0,
        };
        assert!(!rect.contains(-1.0, 0.0));
        assert!(!rect.contains(0.0, 1000.0));
    }

    #[tokio::test]
    async fn mitm_packet_dispatch_uses_registered_uplink() {
        let ctx = AutoplayContext::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        *ctx.mitm_packet_tx.write().await = Some(tx);

        assert!(ctx.send_mitm_packet(&[1, 2, 3]).await.unwrap().is_ok());
        assert_eq!(rx.recv().await, Some(vec![1, 2, 3]));
    }

    #[test]
    fn auto_join_stops_when_either_limit_is_reached() {
        let ctx = AutoplayContext::new();
        let mut cfg = crate::config::MajsoulAutoplayConfig {
            auto_join_game: true,
            auto_join_stop_after_games: 2,
            ..Default::default()
        };
        ctx.auto_join_start();
        ctx.auto_join_record_completed();
        assert!(ctx.auto_join_can_join(&cfg));
        ctx.auto_join_record_completed();
        assert!(!ctx.auto_join_can_join(&cfg));
        assert_eq!(
            ctx.auto_join_status(true, &cfg).stop_reason,
            Some(AutoJoinStopReason::GameLimit)
        );

        let ctx = AutoplayContext::new();
        cfg.auto_join_stop_after_games = 0;
        cfg.auto_join_stop_after_minutes = 1;
        {
            let mut state = ctx.auto_join.lock().unwrap();
            state.started_at = Some(Instant::now() - Duration::from_secs(60));
        }
        assert!(!ctx.auto_join_can_join(&cfg));
        assert_eq!(
            ctx.auto_join_status(true, &cfg).stop_reason,
            Some(AutoJoinStopReason::TimeLimit)
        );
    }
}
