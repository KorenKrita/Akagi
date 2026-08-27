//! Server-granted time budget for the current decision window.
//!
//! Majsoul attaches an `OptionalOperationList` to every action that opens a
//! decision window for our seat; its `time_fixed` / `time_add` fields (both
//! **milliseconds** on the wire — unlike `GameDetailRule`, whose same-named
//! fields are seconds) are the base thinking time and the extra time pool
//! granted for that window.
//!
//! The Majsoul bridge is the single writer: on every `ActionPrototype` it
//! either stores the freshly-opened window's budget (operation present and
//! addressed to our seat) or clears the slot (no operation — no window is
//! open). The autoplay manager is the reader. The value is a per-window
//! snapshot straight from the server, never locally accounted, so it cannot
//! drift out of sync across reconnects, spectating or manual takeover.
//!
//! The slot is a `std::sync::RwLock` because the writer runs inside the
//! bridge's synchronous `parse()` path (already under a `std::sync::Mutex`)
//! and the reader only takes a copy; the critical section is a pointer-sized
//! write and is never held across an await.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const INACTIVITY_WARNING_AFTER: Duration = Duration::from_secs(5);

/// Which action type carried the operation list. Debug/telemetry only —
/// the delay model keys off the mjai action, not off this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    /// `ActionNewRound` — the opening hand (dealer's 14-tile window).
    NewRound,
    /// `ActionDealTile` — our own draw.
    DealTile,
    /// `ActionDiscardTile` — a claim window on another seat's discard.
    DiscardTile,
    /// `ActionChiPengGang` — post-call windows (e.g. discard after a call).
    ChiPengGang,
    /// `ActionAnGangAddGang` — chankan windows (ron on a kakan; kokushi
    /// robbing an ankan).
    AnGangAddGang,
    /// `ActionBaBei` (3p) — 胡拔北 windows.
    BaBei,
}

/// Time budget the server granted for the *current* decision window.
#[derive(Debug, Clone, Copy)]
pub struct TimeBudget {
    /// Base thinking time for this window (`operation.time_fixed`), ms.
    pub fixed_ms: u32,
    /// Extra time pool (`operation.time_add`), ms. Whether this is the
    /// remaining bank or a per-window grant is still unverified — treat
    /// it as "may be consumed" and spend it conservatively.
    pub add_ms: u32,
    /// When we decoded the frame that opened the window. Network + proxy
    /// latency is already inside the frame arrival time, so this is the
    /// closest observable point to "server started the clock".
    pub opened_at: Instant,
    /// Which action carried the operation list.
    pub source: BudgetSource,
    /// One-based discard turn (巡目) when this window opened.
    pub jun: u32,
    /// Honba counter for the current hand.
    pub honba: u8,
    /// Round wind (`E`, `S`, `W`, `N`) and one-based hand number.
    pub bakaze: char,
    pub kyoku: u8,
}

impl TimeBudget {
    /// Milliseconds elapsed since the window opened, saturating.
    pub fn elapsed_ms(&self) -> u32 {
        u32::try_from(self.opened_at.elapsed().as_millis()).unwrap_or(u32::MAX)
    }
}

/// Shared slot: bridge writes, autoplay manager reads.
pub type SharedTimeBudget = Arc<RwLock<Option<TimeBudget>>>;

/// Fresh empty slot.
pub fn new_shared() -> SharedTimeBudget {
    Arc::new(RwLock::new(None))
}

/// Log once if this exact decision window is still open after five seconds.
pub(crate) fn warn_if_still_open(slot: SharedTimeBudget, budget: TimeBudget) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        tokio::time::sleep(INACTIVITY_WARNING_AFTER.saturating_sub(budget.opened_at.elapsed()))
            .await;
        if window_is_still_open(&slot, budget.opened_at) {
            tracing::warn!(
                target: "akagi::autoplay",
                source = ?budget.source,
                round = %format_args!("{}{}", wind_zh(budget.bakaze), budget.kyoku),
                jun = budget.jun,
                honba = budget.honba,
                "decision window has been open for 5 seconds without an operation"
            );
        }
    });
}

fn wind_zh(bakaze: char) -> char {
    match bakaze {
        'E' => '东',
        'S' => '南',
        'W' => '西',
        'N' => '北',
        _ => '?',
    }
}

fn window_is_still_open(slot: &SharedTimeBudget, opened_at: Instant) -> bool {
    slot.read()
        .ok()
        .and_then(|guard| *guard)
        .is_some_and(|current| current.opened_at == opened_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_open_window_is_considered_inactive() {
        let slot = new_shared();
        let opened_at = Instant::now();
        *slot.write().unwrap() = Some(TimeBudget {
            fixed_ms: 5_000,
            add_ms: 0,
            opened_at,
            source: BudgetSource::DealTile,
            jun: 3,
            honba: 2,
            bakaze: 'S',
            kyoku: 1,
        });

        assert!(window_is_still_open(&slot, opened_at));
        assert!(!window_is_still_open(
            &slot,
            opened_at.checked_sub(Duration::from_millis(1)).unwrap()
        ));
        *slot.write().unwrap() = None;
        assert!(!window_is_still_open(&slot, opened_at));
    }
}
