//! Copilot-style weighted discard sampling for external mjai bots.
//!
//! The mirror of `native_bot::selection` (kagami's port of
//! MahjongCopilot's `ai_randomize_choice`), applied at the
//! [`BotManager`](crate::bot::manager::BotManager) layer to **external
//! subprocess bots**: when the bot's chosen action is a discard and
//! `bot.selection.randomize_level > 0`, optionally play a DIFFERENT tile
//! drawn from the bot's top-3 discard candidates, weighted by the bot's
//! own policy probabilities.
//!
//! The native (in-process) bot already samples inside
//! `native_bot::Engine::decide` — the manager skips it by reserved name
//! so the two paths never double-apply.
//!
//! Candidate probabilities come from `softmax(q_values, temperature =
//! 0.3)` over the legal discard actions (mask bits 0..=36: 34 plain
//! tiles + 3 red fives; bits 37..=45 are declarations and are never
//! sampled) — the same transform Mortal's `meta_show.py` applies for the
//! HUD's top-K list, so the sampling distribution matches what the HUD
//! displays.
//!
//! Discards only: reach / chi / pon / kan / hora / ryukyoku / kita
//! responses pass through untouched. When the sampled tile differs from
//! the bot's pick, the mjai `tsumogiri` flag is recomputed against the
//! seat's last self-draw so autoplay clicks and packets stay truthful.

use crate::bot::types::BotResponse;
use crate::schema::MjaiEvent;

const TEMPERATURE: f64 = 0.3;
const TOP_N: usize = 3;
/// Action indices 0..=36 are discards (34 plain tiles + 3 red fives).
const MAX_DISCARD_INDEX: usize = 37;

/// mjai tile labels for action indices 0..=36 — the same table
/// Mortal's `meta_show.py::ACTION_LABELS_4P` uses.
const TILE_LABELS: [&str; 37] = [
    "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", //
    "1p", "2p", "3p", "4p", "5p", "6p", "7p", "8p", "9p", //
    "1s", "2s", "3s", "4s", "5s", "6s", "7s", "8s", "9s", //
    "E", "S", "W", "N", "P", "F", "C", //
    "5mr", "5pr", "5sr",
];

/// If applicable, sample a replacement discard into `resp`.
///
/// Returns `Some((from, to))` when the discard was actually replaced
/// (for tracing); `None` means "nothing to do" for any reason — wrong
/// action kind, level off, missing `q_values`/`mask_bits`, fewer than
/// two legal discards, or the sample happened to land on the bot's own
/// pick.
pub fn apply_discard_sampling(
    resp: &mut BotResponse,
    level: u8,
    last_self_tsumo: Option<&str>,
) -> Option<(String, String)> {
    if level == 0 {
        return None;
    }
    let MjaiEvent::Dahai {
        pai: ref mut action_pai,
        ref mut tsumogiri,
        ..
    } = resp.action
    else {
        return None;
    };
    let meta = resp.meta.as_ref()?;
    let q_values = meta.get("q_values")?.as_array()?;
    let mask_bits = meta.get("mask_bits")?.as_u64()?;

    // Legal discard candidates: (action_index, q_value).
    let cands: Vec<(usize, f64)> = (0..MAX_DISCARD_INDEX.min(q_values.len()))
        .filter(|i| (mask_bits >> i) & 1 == 1)
        .filter_map(|i| q_values[i].as_f64().map(|q| (i, q)))
        .collect();
    if cands.len() < 2 {
        return None;
    }

    let probs = softmax(&cands.iter().map(|(_, q)| *q).collect::<Vec<_>>());
    // Best-first order, truncated to the top N candidates.
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| probs[b].total_cmp(&probs[a]));
    order.truncate(TOP_N);
    if order.len() < 2 {
        return None;
    }

    let level = level.clamp(1, 5);
    let power = 1.0 / (0.2 * f64::from(level));
    let weights: Vec<f64> = order.iter().map(|&i| probs[i].powf(power)).collect();
    let total: f64 = weights.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }

    let mut rng = rand::rng();
    let r = rand::Rng::random::<f64>(&mut rng) * total;
    let mut acc = 0.0;
    let mut chosen = order.len() - 1;
    for (k, w) in weights.iter().enumerate() {
        acc += w;
        if r <= acc {
            chosen = k;
            break;
        }
    }
    let action_index = cands[order[chosen]].0;
    let pai = TILE_LABELS[action_index];

    let from = action_pai.clone();
    if pai == from {
        // A legitimate sample that landed on the bot's own pick.
        return None;
    }
    let draw = last_self_tsumo.unwrap_or("");
    *action_pai = pai.to_string();
    *tsumogiri = is_tsumogiri(pai, draw);
    Some((from, pai.to_string()))
}

/// A discard of the tile just drawn is tsumogiri; anything else is a
/// tedashi. A red five drawn plain (`5m`) still counts as tsumogiri when
/// the red copy is discarded (and vice versa: a red draw discarded as
/// the plain five is a tedashi for wire purposes — the red tile stayed
/// in hand).
fn is_tsumogiri(pai: &str, draw: &str) -> bool {
    if pai == draw {
        return true;
    }
    matches!(draw, "5m" | "5p" | "5s") && pai.len() == 3 && pai == red_of(draw)
}

fn red_of(plain_five: &str) -> String {
    let mut s = plain_five.to_string();
    s.push('r');
    s
}

fn softmax(values: &[f64]) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let scaled: Vec<f64> = values.iter().map(|v| v / TEMPERATURE).collect();
    let m = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = scaled.iter().map(|v| (v - m).exp()).collect();
    let s: f64 = exps.iter().sum();
    if s <= 0.0 {
        return vec![0.0; values.len()];
    }
    exps.iter().map(|e| e / s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dahai(pai: &str, tsumogiri: bool) -> BotResponse {
        BotResponse {
            action: MjaiEvent::Dahai {
                actor: 0,
                pai: pai.into(),
                tsumogiri,
            },
            meta: None,
        }
    }

    fn meta(q: &[f64], mask: u64) -> serde_json::Value {
        json!({ "q_values": q, "mask_bits": mask })
    }

    #[test]
    fn level_zero_is_a_no_op() {
        let mut resp = dahai("1m", false);
        assert!(apply_discard_sampling(&mut resp, 0, None).is_none());
    }

    #[test]
    fn non_discard_actions_pass_through() {
        let q = vec![1.0_f64; 46];
        let mut resp = BotResponse {
            action: MjaiEvent::Reach {
                actor: 0,
                pai: None,
            },
            meta: Some(json!({ "q_values": q, "mask_bits": u64::MAX })),
        };
        assert!(apply_discard_sampling(&mut resp, 3, None).is_none());
    }

    #[test]
    fn missing_meta_is_a_no_op() {
        let mut resp = dahai("1m", false);
        assert!(apply_discard_sampling(&mut resp, 3, None).is_none());
        resp.meta = Some(json!({ "q_values": vec![1.0_f64; 46] }));
        assert!(apply_discard_sampling(&mut resp, 3, None).is_none());
    }

    #[test]
    fn only_top_three_are_ever_chosen() {
        let mut q = vec![0.0; 46];
        q[0] = 4.0;
        q[1] = 3.0;
        q[2] = 2.0;
        q[3] = 1.0;
        for _ in 0..2000 {
            let mut resp = dahai("1m", false);
            resp.meta = Some(meta(&q, 0b1111));
            apply_discard_sampling(&mut resp, 5, None);
            let MjaiEvent::Dahai { pai, .. } = resp.action else {
                panic!("not a dahai");
            };
            assert!(matches!(pai.as_str(), "1m" | "2m" | "3m"), "{pai}");
        }
    }

    #[test]
    fn masked_out_actions_are_never_chosen() {
        let mut q = vec![0.0; 46];
        q[0] = 9.0; // best q, but masked out
        q[1] = 2.0;
        q[2] = 1.0;
        for _ in 0..500 {
            let mut resp = dahai("2m", false);
            resp.meta = Some(meta(&q, 0b110));
            apply_discard_sampling(&mut resp, 5, None);
            let MjaiEvent::Dahai { pai, .. } = resp.action else {
                panic!("not a dahai");
            };
            assert!(matches!(pai.as_str(), "2m" | "3m"), "{pai}");
        }
    }

    #[test]
    fn declarations_are_never_chosen() {
        let mut q = vec![0.0; 46];
        q[0] = 1.0;
        q[1] = 2.0;
        q[2] = 3.0;
        q[37] = 9.0; // reach — huge q, masked legal, must not be sampled
        for _ in 0..1000 {
            let mut resp = dahai("3m", false);
            resp.meta = Some(meta(&q, 0b111 | (1 << 37)));
            apply_discard_sampling(&mut resp, 5, None);
            let MjaiEvent::Dahai { pai, .. } = resp.action else {
                panic!("not a dahai");
            };
            assert!(matches!(pai.as_str(), "1m" | "2m" | "3m"), "{pai}");
        }
    }

    #[test]
    fn replacement_recomputes_tsumogiri() {
        let mut q = vec![0.0; 46];
        q[0] = 5.0; // model wants to discard 1m
        q[1] = 4.0; // 2m is a plausible sample
                    // Force the 2m sample by making it overwhelmingly likely at
                    // level 5 is impossible (probabilities are ~0.75/0.25), so run
                    // the loop until a replacement lands and check the flag then.
        let mut checked_replacement = false;
        for _ in 0..4000 {
            let mut resp = dahai("1m", false);
            resp.meta = Some(meta(&q, 0b11));
            if let Some((from, to)) = apply_discard_sampling(&mut resp, 5, Some("2m")) {
                assert_eq!(from, "1m");
                assert_eq!(to, "2m");
                let MjaiEvent::Dahai { pai, tsumogiri, .. } = resp.action else {
                    panic!("not a dahai");
                };
                assert_eq!(pai, "2m");
                assert!(tsumogiri, "discarding the draw must be tsumogiri");
                checked_replacement = true;
                break;
            }
        }
        assert!(checked_replacement, "2m never sampled in 4000 draws");
        // And a non-draw tile is a tedashi.
        for _ in 0..4000 {
            let mut resp = dahai("1m", false);
            resp.meta = Some(meta(&q, 0b11));
            if apply_discard_sampling(&mut resp, 5, Some("9s")).is_some() {
                let MjaiEvent::Dahai { tsumogiri, .. } = resp.action else {
                    panic!("not a dahai");
                };
                assert!(!tsumogiri);
                break;
            }
        }
    }

    #[test]
    fn red_five_draw_counts_for_red_discard() {
        // Draw the plain 5m; discarding the red 5mr is still a
        // tsumogiri-shaped discard of the draw.
        assert!(is_tsumogiri("5mr", "5m"));
        assert!(is_tsumogiri("5m", "5m"));
        assert!(!is_tsumogiri("5m", "5mr"), "plain discard of a red draw");
        assert!(!is_tsumogiri("4m", "5m"));
        assert!(is_tsumogiri("5pr", "5p"));
    }

    #[test]
    fn softmax_matches_reference() {
        let p = softmax(&[1.0, 2.0, 3.0]);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(p[2] > p[1] && p[1] > p[0]);
        let m = 3.0_f64 / TEMPERATURE;
        let exps: Vec<f64> = [1.0, 2.0, 3.0]
            .iter()
            .map(|v| (v / TEMPERATURE - m).exp())
            .collect();
        let s: f64 = exps.iter().sum();
        for (a, b) in p.iter().zip(exps.iter().map(|e| e / s)) {
            assert!((a - b).abs() < 1e-12);
        }
    }
}
