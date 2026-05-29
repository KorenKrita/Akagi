//! Per-game `MjaiEvent` buffer + finalisation on `EndGame`.
//!
//! `drive_loop` subscribes to the shared `MjaiBus`, accumulates events
//! into an internal buffer, and on `EndGame` runs the aggregator and
//! writes the result via `HistoryStore`. A `StartGame` always resets
//! the buffer first — defensive in case the previous game ended without
//! `EndGame` (mid-session disconnect, app restart mid-game, etc.). Any
//! buffer that never sees `EndGame` is silently dropped on the next
//! `StartGame` or on shutdown — that is the contract for "complete game
//! only" recording.

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use tokio::sync::broadcast::{error::RecvError, Receiver};
use tracing::{info, warn};
use ulid::Ulid;

use crate::event_bus::HistoryBus;
use crate::history::aggregator::{aggregate, AggregateInput};
use crate::history::store::HistoryStore;
use crate::schema::{HistoryEvent, MjaiEvent, Platform};

/// Shared cell holding the platform tag every newly-finalised `GameRecord`
/// is stamped with. `update_config` writes here when the user switches
/// bridges so subsequent games persist with the correct platform without
/// requiring an app relaunch. Reads are sync (cheap, non-blocking) because
/// the recorder finalises inside a Tokio task.
pub type SharedPlatform = Arc<RwLock<Platform>>;

/// Convenience constructor for [`SharedPlatform`].
pub fn shared_platform(initial: Platform) -> SharedPlatform {
    Arc::new(RwLock::new(initial))
}

/// Hard cap on per-game buffer size. Defends against runaway streams
/// (a normal hanchan is ~1500 events; tonpuu ~600). On overflow the
/// buffer is cleared and the game is forfeited from history.
const MAX_EVENTS_PER_GAME: usize = 5_000;

/// Subscribe to `mjai_rx` and finalise complete games into `store`.
/// Drops out cleanly when the broadcast channel is closed.
pub async fn drive_loop(
    store: Arc<HistoryStore>,
    history_bus: HistoryBus,
    platform: SharedPlatform,
    mut mjai_rx: Receiver<MjaiEvent>,
) {
    let mut state = RecorderState::new(platform);
    loop {
        match mjai_rx.recv().await {
            Ok(ev) => state.handle(ev, &store, &history_bus),
            Err(RecvError::Lagged(n)) => {
                // Lagged consumers must reset — we've lost events that
                // belonged to the in-flight game.
                warn!(
                    target: "akagi::history",
                    "history recorder lagged by {n}; dropping in-flight buffer"
                );
                state.reset();
            }
            Err(RecvError::Closed) => {
                info!(target: "akagi::history", "history recorder shutting down");
                return;
            }
        }
    }
}

struct RecorderState {
    /// Read at finalise time so a runtime platform change picks up on the
    /// next completed game. Cloned out under a sync read lock to avoid
    /// holding the lock across the aggregator call.
    platform: SharedPlatform,
    /// In-flight buffer; `None` until the first `start_game` arrives.
    buf: Option<Vec<MjaiEvent>>,
    /// Wall-clock time of the active buffer's first event.
    started_at: Option<DateTime<Utc>>,
    /// True once a `start_game` has been seen for the current buffer.
    /// We refuse to finalise a buffer that never had a `start_game`.
    has_start: bool,
    /// True once the buffer has overflown; subsequent events are
    /// ignored until the next `start_game`.
    overflown: bool,
}

impl RecorderState {
    fn new(platform: SharedPlatform) -> Self {
        Self {
            platform,
            buf: None,
            started_at: None,
            has_start: false,
            overflown: false,
        }
    }

    fn reset(&mut self) {
        self.buf = None;
        self.started_at = None;
        self.has_start = false;
        self.overflown = false;
    }

    fn handle(&mut self, ev: MjaiEvent, store: &HistoryStore, bus: &HistoryBus) {
        match &ev {
            MjaiEvent::StartGame { .. } => {
                self.buf = Some(Vec::with_capacity(1024));
                self.started_at = Some(Utc::now());
                self.has_start = true;
                self.overflown = false;
                self.push(ev);
            }
            MjaiEvent::EndGame => {
                self.push(ev);
                self.finalise(store, bus);
                self.reset();
            }
            _ => self.push(ev),
        }
    }

    fn push(&mut self, ev: MjaiEvent) {
        if self.overflown {
            return;
        }
        let Some(buf) = &mut self.buf else { return };
        if buf.len() >= MAX_EVENTS_PER_GAME {
            warn!(
                target: "akagi::history",
                "buffer exceeded {MAX_EVENTS_PER_GAME} events; abandoning game"
            );
            self.overflown = true;
            buf.clear();
            return;
        }
        buf.push(ev);
    }

    fn finalise(&mut self, store: &HistoryStore, bus: &HistoryBus) {
        if !self.has_start || self.overflown {
            return;
        }
        let Some(events) = self.buf.take() else {
            return;
        };
        let started_at = self.started_at.unwrap_or_else(Utc::now);
        let ended_at = Utc::now();
        let id = Ulid::new().to_string();

        let platform = *self
            .platform
            .read()
            .expect("history platform lock poisoned");
        let Some(record) = aggregate(AggregateInput {
            events: &events,
            platform,
            started_at,
            ended_at,
            id: id.clone(),
        }) else {
            warn!(
                target: "akagi::history",
                "aggregator rejected buffer (missing start_game?); dropping"
            );
            return;
        };

        if let Err(e) = store.append(&record, &events) {
            warn!(
                target: "akagi::history",
                "failed to persist GameRecord {id}: {e:#}"
            );
            return;
        }

        info!(
            target: "akagi::history",
            "recorded game {id} (rank {:?}, Δ{:?})",
            record.our_rank,
            record.our_delta
        );

        // Best-effort emit; if no subscribers, broadcast::send returns
        // Err but we don't care.
        let _ = bus.send(HistoryEvent::Recorded {
            record: Box::new(record),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::{HistoryBus, DEFAULT_CAPACITY};
    use crate::schema::{HistoryFilter, MjaiEvent};
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn start_game() -> MjaiEvent {
        MjaiEvent::StartGame {
            names: vec!["A".into(), "B".into(), "C".into(), "D".into()],
            kyoku_first: Some(0),
            aka_flag: Some(true),
            id: Some(0),
            num_players: 4,
        }
    }

    fn start_kyoku() -> MjaiEvent {
        MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "1m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25000, 25000, 25000, 25000],
            tehais: vec![vec![]; 4],
            num_players: 4,
        }
    }

    #[tokio::test]
    async fn complete_game_writes_record() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(HistoryStore::new(tmp.path().to_path_buf()).unwrap());
        let (tx, rx) = broadcast::channel::<MjaiEvent>(DEFAULT_CAPACITY);
        let (history_tx, _history_rx): (HistoryBus, _) = broadcast::channel(8);

        let store_clone = store.clone();
        let bus_clone = history_tx.clone();
        let handle = tokio::spawn(async move {
            drive_loop(
                store_clone,
                bus_clone,
                shared_platform(Platform::Majsoul),
                rx,
            )
            .await
        });

        tx.send(start_game()).unwrap();
        tx.send(start_kyoku()).unwrap();
        tx.send(MjaiEvent::Hora {
            actor: 0,
            target: 1,
            deltas: Some(vec![8000, -8000, 0, 0]),
            ura_markers: None,
        })
        .unwrap();
        tx.send(MjaiEvent::EndKyoku).unwrap();
        tx.send(MjaiEvent::EndGame).unwrap();

        // Give the loop a tick to drain.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let records = store.list(&HistoryFilter::default(), 100, 0).unwrap();
        assert_eq!(records.len(), 1, "exactly one record after end_game");
        assert_eq!(records[0].our_rank, Some(1));
        assert_eq!(records[0].our_delta, Some(8000));

        drop(tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn disconnect_without_end_game_drops_buffer() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(HistoryStore::new(tmp.path().to_path_buf()).unwrap());
        let (tx, rx) = broadcast::channel::<MjaiEvent>(DEFAULT_CAPACITY);
        let (history_tx, _history_rx): (HistoryBus, _) = broadcast::channel(8);

        let store_clone = store.clone();
        let bus_clone = history_tx.clone();
        let handle = tokio::spawn(async move {
            drive_loop(
                store_clone,
                bus_clone,
                shared_platform(Platform::Majsoul),
                rx,
            )
            .await
        });

        tx.send(start_game()).unwrap();
        tx.send(start_kyoku()).unwrap();
        tx.send(MjaiEvent::Hora {
            actor: 0,
            target: 1,
            deltas: Some(vec![8000, -8000, 0, 0]),
            ura_markers: None,
        })
        .unwrap();
        // Channel closes without an EndGame.
        drop(tx);
        let _ = handle.await;

        let records = store.list(&HistoryFilter::default(), 100, 0).unwrap();
        assert!(records.is_empty(), "no record without end_game");
    }

    #[tokio::test]
    async fn second_start_game_resets_buffer() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(HistoryStore::new(tmp.path().to_path_buf()).unwrap());
        let (tx, rx) = broadcast::channel::<MjaiEvent>(DEFAULT_CAPACITY);
        let (history_tx, _history_rx): (HistoryBus, _) = broadcast::channel(8);

        let store_clone = store.clone();
        let bus_clone = history_tx.clone();
        let handle = tokio::spawn(async move {
            drive_loop(
                store_clone,
                bus_clone,
                shared_platform(Platform::Majsoul),
                rx,
            )
            .await
        });

        // First game starts but never ends.
        tx.send(start_game()).unwrap();
        tx.send(start_kyoku()).unwrap();

        // Second game starts cleanly and ends.
        tx.send(start_game()).unwrap();
        tx.send(start_kyoku()).unwrap();
        tx.send(MjaiEvent::Hora {
            actor: 0,
            target: 1,
            deltas: Some(vec![8000, -8000, 0, 0]),
            ura_markers: None,
        })
        .unwrap();
        tx.send(MjaiEvent::EndGame).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(tx);
        let _ = handle.await;

        let records = store.list(&HistoryFilter::default(), 100, 0).unwrap();
        assert_eq!(records.len(), 1, "only the cleanly-ended second game");
    }
}
