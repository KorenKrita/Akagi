//! Adapters that turn a riichienv-core observation into our [`EncInput`].
//!
//! Used identically by the extractor and by live inference, which is what
//! guarantees train/inference feature parity.

use riichienv_core::action::Action;
use riichienv_core::observation::Observation;
use riichienv_core::observation_3p::Observation3P;
use riichienv_core::state::GameState;
use riichienv_core::state_3p::GameState3P;

use crate::obs::{EncInput, SeatFeat};

/// Snapshot `(encoded obs, legal actions)` for `seat` from a 4-player engine.
/// Shared by the extractor and live inference so features always match.
pub fn obs_and_legal_4p(state: &mut GameState, seat: u8) -> (Vec<f32>, Vec<Action>) {
    let turn = state.turn_count;
    let obs = state.get_observation(seat);
    let legal = obs.legal_actions_method();
    (enc_input_4p(&obs, turn).encode(), legal)
}

/// Snapshot `(encoded obs, legal actions)` for `seat` from a 3-player engine.
/// The `Action3P` legal set is unwrapped to plain `Action`s (their inner value).
pub fn obs_and_legal_3p(state: &mut GameState3P, seat: u8) -> (Vec<f32>, Vec<Action>) {
    let turn = state.turn_count;
    let kita: [u8; 3] = std::array::from_fn(|i| state.players[i].kita_tiles.len() as u8);
    let obs = state.get_observation(seat);
    let legal: Vec<Action> = obs
        .legal_actions_method()
        .into_iter()
        .map(|a| a.0)
        .collect();
    (enc_input_3p(&obs, turn, kita).encode(), legal)
}

/// Build an [`EncInput`] from a 4-player observation.
///
/// `turn_count` is read from the owning `GameState` (not present on the
/// observation). Seats are placed in relative order (index 0 = deciding player).
pub fn enc_input_4p(obs: &Observation, turn_count: u32) -> EncInput {
    let pid = obs.player_id as usize;
    let oya = obs.oya as usize;

    let seats: Vec<SeatFeat> = (0..4)
        .map(|k| {
            let rel = (pid + k) % 4;
            SeatFeat {
                discards: obs.discards[rel].iter().map(|&t| t as u8).collect(),
                meld_tiles: obs.melds[rel]
                    .iter()
                    .flat_map(|m| m.tiles.iter().copied())
                    .collect(),
                riichi_declared: obs.riichi_declared[rel],
                riichi_tile: obs.riichi_sutehais[rel],
                score: obs.scores[rel],
                kita_count: 0,
            }
        })
        .collect();

    EncInput {
        num_players: 4,
        hand: obs.hands[pid].iter().map(|&t| t as u8).collect(),
        drawn_tile: obs.drawn_tile,
        waits: obs.waits.clone(),
        is_tenpai: obs.is_tenpai,
        dora_indicators: obs.dora_indicators.iter().map(|&t| t as u8).collect(),
        seats,
        last_discard: obs.last_discard.map(|t| t as u8),
        round_wind: obs.round_wind,
        seat_wind: ((pid + 4 - oya) % 4) as u8,
        honba: obs.honba,
        riichi_sticks: obs.riichi_sticks,
        turn_count,
        is_dealer: pid == oya,
        kyoku_index: obs.kyoku_index,
        self_riichi: obs.riichi_declared[pid],
    }
}

/// Build an [`EncInput`] from a 3-player (sanma) observation.
///
/// `kita_counts` holds each seat's nukidora (kita) count in **absolute** seat
/// order; it is not carried on the observation, so the caller reads it from the
/// owning `GameState3P`.
pub fn enc_input_3p(obs: &Observation3P, turn_count: u32, kita_counts: [u8; 3]) -> EncInput {
    let pid = obs.player_id as usize;
    let oya = obs.oya as usize;

    let seats: Vec<SeatFeat> = (0..3)
        .map(|k| {
            let rel = (pid + k) % 3;
            SeatFeat {
                discards: obs.discards[rel].iter().map(|&t| t as u8).collect(),
                meld_tiles: obs.melds[rel]
                    .iter()
                    .flat_map(|m| m.tiles.iter().copied())
                    .collect(),
                riichi_declared: obs.riichi_declared[rel],
                riichi_tile: obs.riichi_sutehais[rel],
                score: obs.scores[rel],
                kita_count: kita_counts[rel],
            }
        })
        .collect();

    EncInput {
        num_players: 3,
        hand: obs.hands[pid].iter().map(|&t| t as u8).collect(),
        drawn_tile: obs.drawn_tile,
        waits: obs.waits.clone(),
        is_tenpai: obs.is_tenpai,
        dora_indicators: obs.dora_indicators.iter().map(|&t| t as u8).collect(),
        seats,
        last_discard: obs.last_discard.map(|t| t as u8),
        round_wind: obs.round_wind,
        seat_wind: ((pid + 3 - oya) % 3) as u8,
        honba: obs.honba,
        riichi_sticks: obs.riichi_sticks,
        turn_count,
        is_dealer: pid == oya,
        kyoku_index: obs.kyoku_index,
        self_riichi: obs.riichi_declared[pid],
    }
}
