//! Platform-agnostic autoplay surface.
//!
//! `PlatformAutoplay` is the only thing `AutoplayManager` knows about —
//! by holding `Arc<dyn PlatformAutoplay>` it can drop in a Tenhou impl
//! later (DOM-based clicks or direct WS inject) without touching the
//! manager's bus subscription / timing logic.

use crate::autoplay::delay::{BudgetSnapshot, DecisionProbs};
use crate::config::{DelayModelConfig, MajsoulAutoplayConfig};
use crate::game_state::snapshot::GameStateSnapshot;
use crate::schema::MjaiEvent;
use riichienv_core::action::Action;

/// One step in the click sequence the manager will execute.
///
/// The 16:9-normalised coordinates match the convention used by the
/// original Akagi Python autoplay `LOCATION` table.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Click at a normalised 16:9 point on the game canvas.
    Click { x_norm: f64, y_norm: f64 },
    /// Pause for `duration_ms` before the next step. Used for the
    /// pre-click "thinking" delay and the inter-click gap inside one
    /// action.
    Sleep { duration_ms: u32 },
    /// Click the first DOM element the page has out of `selectors`.
    ///
    /// Tenhou wires its own action buttons through a document-level click
    /// listener keyed on `name="c<handler>-<arg>"`, so dispatching a real
    /// click on one runs the client's own handler — the action is performed
    /// exactly as if the user had pressed it, with none of the coordinate
    /// guessing a canvas needs. `label` is for logs only.
    ///
    /// A list, because some actions are offered under more than one slot and
    /// which ones exist depends on the hand: the pon that keeps a red five
    /// out of the meld is only drawn when there is a red five to keep. The
    /// order is the caller's preference, not the document's.
    DomClick {
        selectors: Vec<String>,
        label: String,
    },
    /// Block until the client is ready to take input, then continue.
    ///
    /// Frame arrival is not the start of the turn. Tenhou's server can send
    /// several seats' actions at once — against millisecond-fast opponents it
    /// routinely does — while the client spends seconds animating them, and
    /// only then draws the buttons and starts its clock. Everything timed
    /// from frame arrival is timed from the wrong instant.
    AwaitReady { timeout_ms: u32 },
    /// Discard the tile with this Tenhou index (`0..=135`) through the
    /// client's own handler.
    ///
    /// Not a position: the client's discard entry point is addressed by tile,
    /// so it resolves the rest itself — including where the tile currently
    /// sits, which is why calls moving the hand cannot affect this.
    Discard { tile_index: u32 },
}

/// State machine for handling Majsoul's fused reach+discard: the platform
/// emits one reach action that combines the declaration and the discard,
/// so we click reach first, await the bot's follow-up `Dahai`, then click
/// the discard tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReachState {
    #[default]
    Idle,
    /// Path B: we clicked the reach button and injected a synthetic
    /// `Reach` event into MjaiBus. The next `Dahai` from the bot is the
    /// riichi tile and must be clicked even though the bot didn't pre-fill
    /// `Reach.pai`.
    AwaitingDahai,
}

/// Everything the platform impl needs to translate one bot decision
/// into a concrete click sequence.
pub struct ActionContext<'a> {
    /// The bot's chosen action (from `BotResponseBus`).
    pub action: &'a MjaiEvent,
    /// Live game state from the riichi engine.
    pub snapshot: &'a GameStateSnapshot,
    /// Currently legal actions for `our_seat`, sourced from the riichi
    /// engine's `_get_legal_actions_internal`. The platform impl uses
    /// this to:
    /// - decide which action button (chi/pon/kan/...) is in which
    ///   on-screen position, by intersecting with the platform's
    ///   priority table;
    /// - enumerate chi/pon/kan candidate combinations when the bot's
    ///   action is ambiguous (multiple `consume_tiles`).
    pub legal_actions: &'a [Action],
    /// Bot's seat.
    pub our_seat: u8,
    /// The most recent tile any seat discarded — needed to disambiguate
    /// chi/pon target.
    pub last_kawa_tile: Option<&'a str>,
    /// The tile we drew this turn, if any. Used to detect tsumohai
    /// position when emitting `dahai`.
    pub last_self_tsumo: Option<&'a str>,
    /// True from the moment the server confirms our riichi until the
    /// kyoku ends. While set, dahai clicks are suppressed (Majsoul auto-
    /// discards in riichi mode).
    pub self_riichi_accepted: bool,
    /// Reach two-step state (see [`ReachState`]).
    pub reach_state: ReachState,
    /// 3 (sanma) or 4 (yonma).
    pub num_players: u8,
    /// Per-platform config knobs (delays, mouse-move emission, ...).
    pub cfg: &'a MajsoulAutoplayConfig,
    /// Delay-model parameters (see `autoplay::delay`). Owned: it is a
    /// small parameter block cloned per bot response, which keeps test
    /// construction free of an extra borrow.
    pub delay_cfg: DelayModelConfig,
    /// Server time budget for the current decision window, if known.
    /// `None` off-Majsoul or before the first operation list arrives.
    pub budget: Option<BudgetSnapshot>,
    /// Normalized bot confidence for this decision, if the bot's meta
    /// could be interpreted (see `autoplay::delay::probs`).
    pub probs: Option<DecisionProbs>,
    /// User Lua delay policy, when loaded. Consulted by the delay model;
    /// on any script failure the built-in policy runs instead.
    pub delay_script: Option<&'a crate::autoplay::delay::DelayScript>,
    /// Tenhou only: the bridge's hand at Tenhou tile-index resolution plus
    /// the current decision window. `None` on other platforms, and on
    /// Tenhou before the first parsed frame.
    pub tenhou: Option<&'a crate::autoplay::tenhou_state::TenhouState>,
}

/// Output of `PlatformAutoplay::plan`. Captures both the click sequence
/// and the side-effect signal the manager needs to fulfil Path B reach
/// (inject a synthetic `Reach` event back to MjaiBus so the bot emits
/// the follow-up dahai).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanResult {
    pub steps: Vec<Step>,
    /// When `true`, the manager should send a synthetic
    /// `MjaiEvent::Reach { actor: our_seat, pai: None }` onto MjaiBus
    /// after the steps run, and transition `ReachState` to
    /// `AwaitingDahai`. Set only when the bot's `Reach` event omits
    /// `pai` and the platform needs a follow-up dahai.
    pub inject_reach_for_followup: bool,
    /// When `true`, the plan declared riichi without throwing the tile, so
    /// the next bot Dahai for our seat is that tile and should be performed
    /// even though the manager has flipped its `self_riichi_accepted` gate.
    /// Only reached when the bot's `Reach` names no `pai` — with one, both
    /// platforms declare and discard in a single plan.
    pub awaiting_riichi_dahai: bool,
}

pub trait PlatformAutoplay: Send + Sync {
    /// Translate the bot's action into a click sequence + side-effect
    /// hints. Pure: must not perform IO. The manager handles the actual
    /// CDP dispatch and bus injection.
    fn plan(&self, ctx: &ActionContext) -> PlanResult;
}
