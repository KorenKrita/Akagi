//! Replay an mjai game log through the riichienv-core engine and emit
//! `(obs, action, mask)` behavior-cloning samples.
//!
//! One file = one game. We drive the engine exactly like Akagi's live tracker
//! (`apply_mjai_event` for every event) and, **before** applying each event
//! that represents a player's free choice, snapshot `get_observation(actor)` and
//! record the chosen action's id. We also synthesize `Pass` samples: after each
//! discard, every seat that had a legal call but did not claim gets a `Pass`
//! label. Because the same `get_observation` + adapter path is used at inference
//! time, the model always sees identical features.

use riichienv_core::action::{Action, ActionType};
use riichienv_core::parser::mjai_to_tid;
use riichienv_core::replay::MjaiEvent;
use riichienv_core::rule::GameRule;
use riichienv_core::state::GameState;
use riichienv_core::state_3p::GameState3P;

use crate::action_codec::{action_index, pass_index};
use crate::adapt::{obs_and_legal_3p, obs_and_legal_4p};

/// Sink for emitted samples: `(encoded obs [C*T] f32, action id, legal mask)`.
pub type Emit<'a> = dyn FnMut(&[f32], u16, &[u8]) + 'a;

enum Engine {
    Four(Box<GameState>),
    Three(Box<GameState3P>),
}

impl Engine {
    fn new(num_players: u8) -> Self {
        let rule = GameRule::default_tenhou();
        if num_players == 3 {
            Engine::Three(Box::new(GameState3P::new(0, true, None, 0, rule)))
        } else {
            Engine::Four(Box::new(GameState::new(0, true, None, 0, rule)))
        }
    }

    fn num_players(&self) -> u8 {
        match self {
            Engine::Four(_) => 4,
            Engine::Three(_) => 3,
        }
    }

    fn apply(&mut self, ev: MjaiEvent) {
        match self {
            Engine::Four(s) => s.apply_mjai_event(ev),
            Engine::Three(s) => s.apply_mjai_event(ev),
        }
    }

    fn active_players(&self) -> Vec<u8> {
        match self {
            Engine::Four(s) => s.active_players.clone(),
            Engine::Three(s) => s.active_players.clone(),
        }
    }

    /// Snapshot `(encoded obs, legal mask)` for `pid` at the current state.
    fn sample_for(&mut self, pid: u8) -> (Vec<f32>, Vec<u8>) {
        let (obs, legal) = match self {
            Engine::Four(s) => obs_and_legal_4p(s, pid),
            Engine::Three(s) => obs_and_legal_3p(s, pid),
        };
        (obs, crate::action_codec::legal_mask(&legal, self.num_players()))
    }
}

/// The actor + chosen action for events that are a player's free decision.
/// Returns `None` for engine/progression events (tsumo, dora, reach_accepted,
/// start/end, ryukyoku).
fn decision(ev: &MjaiEvent) -> Option<(u8, Action)> {
    let tid = |s: &str| mjai_to_tid(s);
    let tids = |v: &[String]| -> Vec<u8> { v.iter().filter_map(|s| mjai_to_tid(s)).collect() };
    match ev {
        MjaiEvent::Dahai { actor, pai, .. } => Some((
            *actor as u8,
            Action::new(ActionType::Discard, tid(pai), vec![], Some(*actor as u8)),
        )),
        MjaiEvent::Reach { actor } => Some((
            *actor as u8,
            Action::new(ActionType::Riichi, None, vec![], Some(*actor as u8)),
        )),
        MjaiEvent::Pon {
            actor, pai, consumed, ..
        } => Some((
            *actor as u8,
            Action::new(ActionType::Pon, tid(pai), tids(consumed), Some(*actor as u8)),
        )),
        MjaiEvent::Chi {
            actor, pai, consumed, ..
        } => Some((
            *actor as u8,
            Action::new(ActionType::Chi, tid(pai), tids(consumed), Some(*actor as u8)),
        )),
        MjaiEvent::Kan {
            actor, pai, consumed, ..
        } => Some((
            *actor as u8,
            Action::new(
                ActionType::Daiminkan,
                tid(pai),
                tids(consumed),
                Some(*actor as u8),
            ),
        )),
        MjaiEvent::Kakan { actor, pai } => Some((
            *actor as u8,
            Action::new(
                ActionType::Kakan,
                tid(pai),
                tid(pai).into_iter().collect(),
                Some(*actor as u8),
            ),
        )),
        MjaiEvent::Ankan { actor, consumed } => Some((
            *actor as u8,
            Action::new(ActionType::Ankan, None, tids(consumed), Some(*actor as u8)),
        )),
        MjaiEvent::Hora { actor, target, .. } => Some((
            *actor as u8,
            Action::new(
                if actor == target {
                    ActionType::Tsumo
                } else {
                    ActionType::Ron
                },
                None,
                vec![],
                Some(*actor as u8),
            ),
        )),
        MjaiEvent::Kita { actor } => Some((
            *actor as u8,
            Action::new(ActionType::Kita, None, vec![], Some(*actor as u8)),
        )),
        _ => None,
    }
}

/// The claiming seat if this event is a discard-response claim (pon/chi/kan/ron).
fn claimer(ev: &MjaiEvent) -> Option<u8> {
    match ev {
        MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Kan { actor, .. }
        | MjaiEvent::Hora { actor, .. } => Some(*actor as u8),
        _ => None,
    }
}

/// The discarding seat if this event is a discard (opens a response window).
fn discarder(ev: &MjaiEvent) -> Option<u8> {
    match ev {
        MjaiEvent::Dahai { actor, .. } => Some(*actor as u8),
        _ => None,
    }
}

/// Tenhou sanma logs use a 4-seat layout: `start_kyoku`/`hora`/`ryukyoku`
/// carry 4-element `scores`/`tehais`/`delta` (the 4th is a dummy dead seat),
/// which a 3-seat `GameState3P` would index out of bounds. Truncate them to 3.
fn sanitize_3p(ev: &mut MjaiEvent) {
    match ev {
        MjaiEvent::StartKyoku { scores, tehais, .. } => {
            scores.truncate(3);
            tehais.truncate(3);
        }
        MjaiEvent::Hora { delta, scores, .. } => {
            if let Some(d) = delta {
                d.truncate(3);
            }
            if let Some(s) = scores {
                s.truncate(3);
            }
        }
        MjaiEvent::Ryukyoku {
            delta,
            scores,
            tehais,
            ..
        } => {
            if let Some(d) = delta {
                d.truncate(3);
            }
            if let Some(s) = scores {
                s.truncate(3);
            }
            if let Some(t) = tehais {
                t.truncate(3);
            }
        }
        _ => {}
    }
}

/// Replay one game's mjai JSONL `content` and emit samples. Returns the number
/// of samples emitted. Malformed lines are skipped; the caller should guard the
/// whole call with `catch_unwind` for defense against engine panics on rare
/// pathological logs.
pub fn replay_game(content: &str, num_players: u8, emit: &mut Emit) -> usize {
    let mut eng = Engine::new(num_players);
    let np = eng.num_players();
    let pass_idx = pass_index(np) as u16;

    // Tenhou sanma logs spell nukidora (kita) as `nukidora`; riichienv expects
    // `kita`. Rename so the engine applies it (harmless for 4p logs).
    let content = content.replace("\"type\":\"nukidora\"", "\"type\":\"kita\"");

    // Responders snapshotted at the last discard, pending a pass/claim decision.
    let mut window: Vec<(u8, Vec<f32>, Vec<u8>)> = Vec::new();
    let mut count = 0usize;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut ev: MjaiEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if matches!(ev, MjaiEvent::Other) {
            continue;
        }
        if np == 3 {
            sanitize_3p(&mut ev);
        }

        // Close any open response window against this event.
        if !window.is_empty() {
            let who = claimer(&ev);
            for (pid, obs, mask) in window.drain(..) {
                if Some(pid) != who {
                    emit(&obs, pass_idx, &mask);
                    count += 1;
                }
            }
        }

        // Record the actor's own decision (snapshot BEFORE applying).
        if let Some((actor, action)) = decision(&ev) {
            if let Some(idx) = action_index(&action, np) {
                let (obs, mask) = eng.sample_for(actor);
                if mask.get(idx).copied().unwrap_or(0) == 1 {
                    emit(&obs, idx as u16, &mask);
                    count += 1;
                }
            }
        }

        // Note discard/claim before we move `ev` into `apply`.
        let opened_by = discarder(&ev);

        eng.apply(ev);

        // Open a new response window after a discard: snapshot each responder
        // that actually had a call option (legal action other than Pass).
        if let Some(disc) = opened_by {
            for p in eng.active_players() {
                if p == disc {
                    continue;
                }
                let (obs, mask) = eng.sample_for(p);
                let has_call = mask
                    .iter()
                    .enumerate()
                    .any(|(i, &m)| m == 1 && i != pass_idx as usize);
                if has_call {
                    window.push((p, obs, mask));
                }
            }
        }
    }

    // Any responders still pending at end-of-log passed.
    for (_pid, obs, mask) in window.drain(..) {
        emit(&obs, pass_idx, &mask);
        count += 1;
    }

    count
}
