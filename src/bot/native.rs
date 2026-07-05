//! Built-in, in-process bot runner backed by the pure-Rust `native_bot`
//! crate (a small behavior-cloned CNN run via candle — no Python, no
//! libriichi, no subprocess).
//!
//! Unlike [`crate::bot::runner::SubprocessBot`], this runner keeps a live
//! `native_bot::Engine` in-process: each `react()` feeds the batch through the
//! engine's riichienv-core game state and, at our decision points, runs the net
//! to pick a legal action. Bundled model weights are embedded in the binary, so
//! there is no venv, no `uv sync`, and nothing to install.
//!
//! Two reserved bot names select it: [`NATIVE_4P`] (yonma) and [`NATIVE_3P`]
//! (sanma). `BotManager::spawn_runner` recognises them and constructs this
//! runner directly, bypassing the `bot.py`/registry path.

use crate::bot::runner::BotRunner;
use crate::bot::types::BotResponse;
use crate::game_state::convert;
use crate::schema::MjaiEvent;
use anyhow::Result;
use async_trait::async_trait;
use native_bot::engine::{BotAction, Engine};

/// Reserved name for the built-in 4-player bot.
pub const NATIVE_4P: &str = "akagi-native";
/// Reserved name for the built-in 3-player (sanma) bot.
pub const NATIVE_3P: &str = "akagi-native3p";

/// Whether `name` selects the built-in native bot (either mode).
pub fn is_native(name: &str) -> bool {
    name == NATIVE_4P || name == NATIVE_3P
}

/// Display label for a reserved native-bot name, for the Bots UI.
pub fn display_name(name: &str) -> Option<&'static str> {
    match name {
        NATIVE_4P => Some("Akagi (built-in, 4p)"),
        NATIVE_3P => Some("Akagi (built-in, 3p)"),
        _ => None,
    }
}

pub struct NativeBot {
    engine: Engine,
    actor_id: u8,
}

impl NativeBot {
    /// Build the in-process bot for a game of `num_players` with our seat at
    /// `actor_id`, loading the bundled default weights for that mode.
    pub fn new(actor_id: u8, num_players: u8) -> Result<Self> {
        let engine = native_bot::defaults::engine(num_players, actor_id)?;
        Ok(Self { engine, actor_id })
    }
}

#[async_trait]
impl BotRunner for NativeBot {
    async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
        for ev in events {
            // Keep our seat current if a start_game tags a (possibly new) seat.
            if let MjaiEvent::StartGame { id: Some(seat), .. } = ev {
                self.actor_id = *seat;
                self.engine.set_seat(*seat);
            }
            if let Some(ri) = convert::to_riichienv(ev)? {
                self.engine.feed(ri);
            }
        }

        let action = match self.engine.decide()? {
            Some(d) => bot_action_to_mjai(d.action, self.actor_id),
            None => MjaiEvent::None,
        };
        Ok(BotResponse { action, meta: None })
    }

    async fn reset(&mut self) -> Result<()> {
        self.engine.reset();
        Ok(())
    }
}

fn take_n<const N: usize>(v: Vec<String>) -> [String; N] {
    let mut it = v.into_iter();
    std::array::from_fn(|_| it.next().unwrap_or_default())
}

/// Map a schema-agnostic [`BotAction`] to Akagi's `MjaiEvent` reply.
fn bot_action_to_mjai(a: BotAction, actor: u8) -> MjaiEvent {
    match a {
        BotAction::Dahai { pai, tsumogiri } => MjaiEvent::Dahai {
            actor,
            pai,
            tsumogiri,
        },
        BotAction::Reach { pai } => MjaiEvent::Reach {
            actor,
            pai: Some(pai),
        },
        BotAction::Pon {
            target,
            pai,
            consumed,
        } => MjaiEvent::Pon {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Chi {
            target,
            pai,
            consumed,
        } => MjaiEvent::Chi {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Daiminkan {
            target,
            pai,
            consumed,
        } => MjaiEvent::Daiminkan {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Ankan { consumed } => MjaiEvent::Ankan {
            actor,
            consumed: take_n(consumed),
        },
        BotAction::Kakan { pai, consumed } => MjaiEvent::Kakan {
            actor,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Hora { target } => MjaiEvent::Hora {
            actor,
            target,
            deltas: None,
            ura_markers: None,
        },
        BotAction::Kyushu => MjaiEvent::Ryukyoku { deltas: None },
        BotAction::Kita => MjaiEvent::Kita {
            actor,
            pai: Some("N".into()),
        },
        BotAction::Pass => MjaiEvent::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_game_4p(seat: u8) -> MjaiEvent {
        MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(seat),
            num_players: 4,
        }
    }

    fn start_kyoku_4p() -> MjaiEvent {
        let hand: Vec<String> = [
            "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "2m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25000, 25000, 25000, 25000],
            tehais: vec![hand.clone(), hand.clone(), hand.clone(), hand],
            num_players: 4,
        }
    }

    #[tokio::test]
    async fn native_bot_returns_legal_discard_on_own_tsumo() {
        let mut bot = NativeBot::new(0, 4).unwrap();
        // Feed the opening up to our first draw in one batch (as the manager would).
        let resp = bot
            .react(&[
                start_game_4p(0),
                start_kyoku_4p(),
                MjaiEvent::Tsumo {
                    actor: 0,
                    pai: "5p".into(),
                },
            ])
            .await
            .unwrap();
        // On our own tsumo we must act — a discard (or riichi/kan/hora), never None.
        assert!(
            !matches!(resp.action, MjaiEvent::None),
            "bot must act on its own tsumo, got None"
        );
        match resp.action {
            MjaiEvent::Dahai { actor, .. } | MjaiEvent::Reach { actor, .. } => {
                assert_eq!(actor, 0)
            }
            MjaiEvent::Ankan { .. } | MjaiEvent::Kakan { .. } | MjaiEvent::Hora { .. } => {}
            other => panic!("unexpected reply on own turn: {other:?}"),
        }
    }

    #[tokio::test]
    async fn native_bot_passes_when_not_its_turn() {
        let mut bot = NativeBot::new(0, 4).unwrap();
        // Opponent (seat 1) draws and discards; we (seat 0) usually can't act.
        let resp = bot
            .react(&[
                start_game_4p(0),
                start_kyoku_4p(),
                MjaiEvent::Tsumo {
                    actor: 1,
                    pai: "9s".into(),
                },
                MjaiEvent::Dahai {
                    actor: 1,
                    pai: "9s".into(),
                    tsumogiri: true,
                },
            ])
            .await
            .unwrap();
        // Either None (nothing to do) or a legal call — must not be one of our
        // own-turn-only actions.
        assert!(
            !matches!(
                resp.action,
                MjaiEvent::Dahai { .. } | MjaiEvent::Reach { .. }
            ),
            "must not discard on someone else's turn: {:?}",
            resp.action
        );
    }
}
