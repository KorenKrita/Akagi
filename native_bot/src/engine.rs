//! Live inference engine: maintains a riichienv-core game state from mjai
//! events and, at our decision points, runs the CNN to pick a **legal** action.
//!
//! Transport-agnostic: it consumes `riichienv_core::replay::MjaiEvent` and
//! returns a schema-agnostic [`BotAction`] (mjai tile strings), which the
//! Akagi-side `NativeBot` maps to Akagi's own mjai event type. This keeps
//! `native_bot` free of any Akagi dependency.
//!
//! `decide()` is read-only with respect to game *state*: our own chosen action
//! is NOT applied here. In Akagi, every action (ours included) echoes back
//! through the mjai bus and is applied via [`Engine::feed`], so applying it in
//! `decide()` too would double-count.

use anyhow::Result;
use riichienv_core::action::{Action, ActionType};
use riichienv_core::parser::tid_to_mjai;
use riichienv_core::replay::MjaiEvent;
use riichienv_core::rule::GameRule;
use riichienv_core::state::GameState;
use riichienv_core::state_3p::GameState3P;

use crate::action_codec::{pick_by_logits, rank_by_logits};
use crate::adapt::{obs_and_legal_3p, obs_and_legal_4p};
use crate::model::Model;

/// How many ranked candidates the engine surfaces for the HUD's multi-row
/// recommendation card (top-N by policy probability).
const SHOW_TOP_N: usize = 3;

/// A schema-agnostic bot reply, ready to be mapped to Akagi's `MjaiEvent`.
/// All tiles are mjai strings (e.g. `"5mr"`, `"P"`).
#[derive(Debug, Clone, PartialEq)]
pub enum BotAction {
    Dahai { pai: String, tsumogiri: bool },
    /// Riichi declaration; `pai` is the predicted riichi discard (mjai must
    /// carry it or autoplay stalls).
    Reach { pai: String },
    Pon { target: u8, pai: String, consumed: Vec<String> },
    Chi { target: u8, pai: String, consumed: Vec<String> },
    Daiminkan { target: u8, pai: String, consumed: Vec<String> },
    Ankan { consumed: Vec<String> },
    Kakan { pai: String, consumed: Vec<String> },
    /// Ron or tsumo (both are mjai `hora`); `target` is the loser (self for
    /// tsumo, the discarder for ron).
    Hora { target: u8 },
    /// Nine-terminals abortive draw (mjai `ryukyoku`).
    Kyushu,
    /// Kita / nukidora (sanma).
    Kita,
    /// No action this turn.
    Pass,
}

/// Our seat's decision at a given point.
pub struct Decision {
    pub action: BotAction,
    /// Top legal actions ranked by policy probability (best first). `action`
    /// is `candidates[0].0`; the rest are the runner-up recommendations the HUD
    /// shows as a top-N card. Probabilities are a softmax over the legal set.
    pub candidates: Vec<(BotAction, f32)>,
    /// Raw action logits (indexed by the mode's action space).
    pub logits: Vec<f32>,
}

enum Backend {
    Four { state: Box<GameState>, model: Model },
    Three { state: Box<GameState3P>, model: Model },
}

pub struct Engine {
    backend: Backend,
    seat: u8,
    num_players: u8,
}

impl Engine {
    /// Construct for `num_players`, loading weights from a safetensors buffer.
    pub fn new(model_bytes: Vec<u8>, num_players: u8, seat: u8) -> Result<Self> {
        let rule = GameRule::default_tenhou();
        let model = Model::from_safetensors(model_bytes, num_players)?;
        let backend = if num_players == 3 {
            Backend::Three {
                state: Box::new(GameState3P::new(0, true, None, 0, rule)),
                model,
            }
        } else {
            Backend::Four {
                state: Box::new(GameState::new(0, true, None, 0, rule)),
                model,
            }
        };
        Ok(Self {
            backend,
            seat,
            num_players,
        })
    }

    /// Reset to a fresh game while keeping the loaded weights.
    pub fn reset(&mut self) {
        let rule = GameRule::default_tenhou();
        match &mut self.backend {
            Backend::Four { state, .. } => *state = Box::new(GameState::new(0, true, None, 0, rule)),
            Backend::Three { state, .. } => {
                *state = Box::new(GameState3P::new(0, true, None, 0, rule))
            }
        }
    }

    pub fn seat(&self) -> u8 {
        self.seat
    }

    pub fn set_seat(&mut self, seat: u8) {
        self.seat = seat;
    }

    pub fn num_players(&self) -> u8 {
        self.num_players
    }

    /// Drive one mjai event through the engine.
    pub fn feed(&mut self, ev: MjaiEvent) {
        match &mut self.backend {
            Backend::Four { state, .. } => state.apply_mjai_event(ev),
            Backend::Three { state, .. } => state.apply_mjai_event(ev),
        }
    }

    /// Decide our action at the current state. `None` if we currently have no
    /// legal action (not our turn / nothing to respond to).
    pub fn decide(&mut self) -> Result<Option<Decision>> {
        let seat = self.seat;
        let (candidates, logits) = match &mut self.backend {
            Backend::Four { state, model } => {
                let last_pid = state.last_discard.map(|(_, p)| p);
                let drawn = state.drawn_tile;
                let (obs, legal) = obs_and_legal_4p(state, seat);
                if legal.is_empty() {
                    return Ok(None);
                }
                let logits = model.forward_logits(&obs)?;
                let ranked = rank_by_logits(&legal, &logits, 4, SHOW_TOP_N);
                let Some((top, _)) = ranked.first() else {
                    return Ok(None);
                };
                let reach_pai = if top.action_type == ActionType::Riichi {
                    predict_reach_discard(state, model, seat, 4)
                } else {
                    None
                };
                (
                    build_candidates(&ranked, seat, last_pid, drawn, reach_pai),
                    logits,
                )
            }
            Backend::Three { state, model } => {
                let last_pid = state.last_discard.map(|(_, p)| p);
                let drawn = state.drawn_tile;
                let (obs, legal) = obs_and_legal_3p(state, seat);
                if legal.is_empty() {
                    return Ok(None);
                }
                let logits = model.forward_logits(&obs)?;
                let ranked = rank_by_logits(&legal, &logits, 3, SHOW_TOP_N);
                let Some((top, _)) = ranked.first() else {
                    return Ok(None);
                };
                let reach_pai = if top.action_type == ActionType::Riichi {
                    predict_reach_discard_3p(state, model, seat)
                } else {
                    None
                };
                (
                    build_candidates(&ranked, seat, last_pid, drawn, reach_pai),
                    logits,
                )
            }
        };
        let action = candidates[0].0.clone();
        Ok(Some(Decision {
            action,
            candidates,
            logits,
        }))
    }

    /// The tile the local model would discard if it declared riichi right now,
    /// as an mjai string. Used by the API-backed runner as a fallback for the
    /// reach two-step (declare → discard) when the remote follow-up call fails.
    /// `None` if there is no riichi-legal discard from the current state.
    pub fn reach_discard(&mut self) -> Option<String> {
        let seat = self.seat;
        match &mut self.backend {
            Backend::Four { state, model } => {
                predict_reach_discard(state, model, seat, 4).map(tid_to_mjai)
            }
            Backend::Three { state, model } => {
                predict_reach_discard_3p(state, model, seat).map(tid_to_mjai)
            }
        }
    }
}

/// Map a ranked `(Action, prob)` list into displayable `(BotAction, prob)`
/// candidates. The riichi-discard prediction is applied only to the top action
/// (index 0) — predicting it for every runner-up would run the model N extra
/// times for tiles that only decorate an alternative row.
fn build_candidates(
    ranked: &[(Action, f32)],
    seat: u8,
    last_discarder: Option<u8>,
    drawn: Option<u8>,
    reach_pai: Option<u8>,
) -> Vec<(BotAction, f32)> {
    ranked
        .iter()
        .enumerate()
        .map(|(i, (a, p))| {
            let rp = if i == 0 { reach_pai } else { None };
            (build_bot_action(a, seat, last_discarder, drawn, rp), *p)
        })
        .collect()
}

/// Predict the tile we'd discard on a riichi declaration, by advancing a clone
/// past the reach and asking the model for the (riichi-legal) discard.
fn predict_reach_discard(state: &GameState, model: &Model, seat: u8, np: u8) -> Option<u8> {
    let mut clone = state.clone();
    clone.apply_mjai_event(MjaiEvent::Reach {
        actor: seat as usize,
    });
    let (obs, legal) = obs_and_legal_4p(&mut clone, seat);
    if legal.is_empty() {
        return None;
    }
    let logits = model.forward_logits(&obs).ok()?;
    let a = pick_by_logits(&legal, &logits, np)?;
    a.tile
}

fn predict_reach_discard_3p(state: &GameState3P, model: &Model, seat: u8) -> Option<u8> {
    let mut clone = state.clone();
    clone.apply_mjai_event(MjaiEvent::Reach {
        actor: seat as usize,
    });
    let (obs, legal) = obs_and_legal_3p(&mut clone, seat);
    if legal.is_empty() {
        return None;
    }
    let logits = model.forward_logits(&obs).ok()?;
    let a = pick_by_logits(&legal, &logits, 3)?;
    a.tile
}

/// Turn a riichienv `Action` (plus the little bit of table context the reply
/// needs) into a [`BotAction`].
fn build_bot_action(
    a: &Action,
    seat: u8,
    last_discarder: Option<u8>,
    drawn: Option<u8>,
    reach_pai: Option<u8>,
) -> BotAction {
    let consumed = |a: &Action| a.consume_tiles.iter().map(|&t| tid_to_mjai(t)).collect();
    let target = last_discarder.unwrap_or(0);
    match a.action_type {
        ActionType::Discard => match a.tile {
            Some(t) => BotAction::Dahai {
                pai: tid_to_mjai(t),
                tsumogiri: drawn == Some(t),
            },
            None => BotAction::Pass,
        },
        ActionType::Riichi => BotAction::Reach {
            pai: reach_pai.map(tid_to_mjai).unwrap_or_default(),
        },
        ActionType::Pon => BotAction::Pon {
            target,
            pai: a.tile.map(tid_to_mjai).unwrap_or_default(),
            consumed: consumed(a),
        },
        ActionType::Chi => BotAction::Chi {
            target,
            pai: a.tile.map(tid_to_mjai).unwrap_or_default(),
            consumed: consumed(a),
        },
        ActionType::Daiminkan => BotAction::Daiminkan {
            target,
            pai: a.tile.map(tid_to_mjai).unwrap_or_default(),
            consumed: consumed(a),
        },
        ActionType::Ankan => BotAction::Ankan {
            consumed: consumed(a),
        },
        ActionType::Kakan => BotAction::Kakan {
            pai: a.tile.map(tid_to_mjai).unwrap_or_default(),
            consumed: consumed(a),
        },
        ActionType::Tsumo => BotAction::Hora { target: seat },
        ActionType::Ron => BotAction::Hora {
            target: last_discarder.unwrap_or(seat),
        },
        ActionType::KyushuKyuhai => BotAction::Kyushu,
        ActionType::Kita => BotAction::Kita,
        ActionType::Pass => BotAction::Pass,
    }
}
