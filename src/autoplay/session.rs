//! Auto-queue session state: game counting, stop handling, and the
//! start-generation counter the queue task uses to detect "a new game
//! started". Shared (one `Arc`) between the autoplay manager and the
//! IPC start/stop/status commands.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub type SharedAutoplaySession = Arc<AutoplaySession>;

/// Snapshot for the UI (`autoplay_session_status`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AutoplaySessionStatus {
    pub active: bool,
    /// Target game count; `None` = play until stopped.
    pub target_games: Option<u32>,
    pub games_completed: u32,
    #[serde(rename = "stop_reason")]
    pub stop_reason: Option<String>,
    /// Seconds spent waiting in the matchmaking queue, if queueing now.
    pub queue_seconds: Option<u64>,
}

pub struct AutoplaySession {
    active: AtomicBool,
    stop_requested: AtomicBool,
    target_games: AtomicU32,
    /// 0 encodes "no target" (infinite).
    completed: AtomicU32,
    /// Bumped on every `StartGame`; a queueing task snapshots it before
    /// queueing and compares afterwards to tell "a match was found" from
    /// "still waiting".
    start_generation: AtomicU64,
    /// When the current queue attempt started; drives the UI timer.
    queued_since: std::sync::Mutex<Option<std::time::Instant>>,
    stop_reason: std::sync::Mutex<Option<String>>,
}

impl Default for AutoplaySession {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoplaySession {
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            target_games: AtomicU32::new(0),
            completed: AtomicU32::new(0),
            start_generation: AtomicU64::new(0),
            queued_since: std::sync::Mutex::new(None),
            stop_reason: std::sync::Mutex::new(None),
        }
    }

    /// `target_games = None` plays until stopped. `Err` if a session is
    /// already active.
    pub fn start(&self, target_games: Option<u32>) -> Result<(), String> {
        if self
            .active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("an autoplay session is already running".to_string());
        }
        self.stop_requested.store(false, Ordering::SeqCst);
        self.completed.store(0, Ordering::SeqCst);
        self.target_games
            .store(target_games.unwrap_or(0), Ordering::SeqCst);
        *self.stop_reason.lock().expect("stop_reason poisoned") = None;
        Ok(())
    }

    /// Takes effect at the next check: the current game (if any) plays
    /// out and no further match is queued.
    pub fn stop(&self, reason: &str) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.deactivate(reason);
    }

    fn deactivate(&self, reason: &str) {
        self.active.store(false, Ordering::SeqCst);
        *self.stop_reason.lock().expect("stop_reason poisoned") = Some(reason.to_string());
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst) && !self.stop_requested.load(Ordering::SeqCst)
    }

    /// Called by the manager on every `StartGame`.
    pub fn note_game_started(&self) {
        self.start_generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn start_generation(&self) -> u64 {
        self.start_generation.load(Ordering::SeqCst)
    }

    pub fn note_queuing(&self) {
        *self.queued_since.lock().expect("queued_since poisoned") =
            Some(std::time::Instant::now());
    }

    pub fn clear_queue_wait(&self) {
        *self.queued_since.lock().expect("queued_since poisoned") = None;
    }

    /// Called by the manager on `EndGame`; returns whether another game
    /// should be queued.
    pub fn on_game_finished(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let done = self.completed.fetch_add(1, Ordering::SeqCst) + 1;
        let target = self.target_games.load(Ordering::SeqCst);
        if target > 0 && done >= target {
            self.deactivate(&format!("finished {done} of {target} games"));
            return false;
        }
        true
    }

    pub fn status(&self) -> AutoplaySessionStatus {
        AutoplaySessionStatus {
            active: self.is_active(),
            target_games: match self.target_games.load(Ordering::SeqCst) {
                0 => None,
                n => Some(n),
            },
            games_completed: self.completed.load(Ordering::SeqCst),
            stop_reason: self.stop_reason.lock().expect("stop_reason poisoned").clone(),
            queue_seconds: self
                .queued_since
                .lock()
                .expect("queued_since poisoned")
                .map(|t| t.elapsed().as_secs()),
        }
    }
}

/// Uniform in `[inter_delay_ms/2, inter_delay_ms]`.
pub fn inter_game_delay(inter_delay_ms: u32) -> Duration {
    let inter = inter_delay_ms.max(2) as u64;
    let half = inter / 2;
    let r = rand::random::<u64>() % (inter - half);
    Duration::from_millis(half + r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_session_counts_and_stops() {
        let s = AutoplaySession::new();
        s.start(Some(2)).unwrap();
        assert!(s.is_active());
        assert!(s.on_game_finished(), "1 of 2 — queue the next");
        assert!(!s.on_game_finished(), "2 of 2 — stop");
        assert!(!s.is_active());
        assert_eq!(s.status().games_completed, 2);
        assert!(s.status().stop_reason.unwrap().contains("2 of 2"));
    }

    #[test]
    fn infinite_session_runs_until_stopped() {
        let s = AutoplaySession::new();
        s.start(None).unwrap();
        for _ in 0..10 {
            assert!(s.on_game_finished());
        }
        s.stop("stopped by user");
        assert!(!s.is_active());
        assert!(!s.on_game_finished(), "inactive session never queues");
    }

    #[test]
    fn second_start_is_refused_while_active() {
        let s = AutoplaySession::new();
        s.start(None).unwrap();
        assert!(s.start(None).is_err());
        s.stop("done");
        assert!(s.start(Some(3)).is_ok());
        // Restart resets the counters.
        assert_eq!(s.status().games_completed, 0);
        assert!(s.status().stop_reason.is_none());
    }

    #[test]
    fn stop_during_a_game_lets_it_finish() {
        let s = AutoplaySession::new();
        s.start(None).unwrap();
        s.stop_requested.store(true, Ordering::SeqCst);
        assert!(!s.on_game_finished(), "no queue after a mid-game stop");
    }

    #[test]
    fn queue_wait_timer_starts_and_clears() {
        let s = AutoplaySession::new();
        s.start(None).unwrap();
        assert!(s.status().queue_seconds.is_none());
        s.note_queuing();
        assert_eq!(s.status().queue_seconds, Some(0));
        s.clear_queue_wait();
        assert!(s.status().queue_seconds.is_none());
    }
}
