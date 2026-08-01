//! Autoplay: translate bot decisions into UI clicks via CDP.
//!
//! Data flow: bot decision (an `MjaiEvent`) joins the latest
//! `GameStateSnapshot` and `legal_actions` from the `riichienv-core` game
//! state, and the platform impl plans a sequence of click `Step`s. Action
//! availability is sourced from the riichi engine, not from the platform
//! protocol parser, so the same logic works across platforms.
//!
//! Reach is handled in two steps (declare → discard) so the UI's reach
//! popup is acknowledged before the actual discard is clicked.
//!
//! Module layout:
//! - [`context`] — shared state between the chromium capture backend
//!   and this manager (page handle + canvas rect cache).
//! - [`platform`] — `PlatformAutoplay` trait + the click-step types.
//!   Tenhou-ready: a future `tenhou` impl can use a different `Step`
//!   variant for DOM clicks or WebSocket inject.
//! - [`majsoul`] — the production Majsoul implementation: 16:9
//!   coordinate tables ported from the reference Akagi Python file +
//!   plan dispatch covering all mjai action types.
//! - [`cdp_input`] — chromiumoxide wrappers (`dispatch_click`,
//!   `evaluate_canvas_rect`).
//! - [`manager`] — the long-lived `AutoplayManager` task that owns
//!   per-game state and drives the click sequence.
//!
//! Entry point: [`manager::run_autoplay_manager`].

pub mod budget;
pub mod cdp_input;
pub mod context;
pub mod delay;
pub mod majsoul;
pub mod manager;
pub mod platform;

pub use budget::{BudgetSource, SharedTimeBudget, TimeBudget};
pub use context::{AutoplayContext, CanvasRect};
pub use manager::run_autoplay_manager;
pub use platform::{ActionContext, PlanResult, PlatformAutoplay, ReachState, Step};
