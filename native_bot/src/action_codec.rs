//! Action indexing shared by extraction, training, and inference.
//!
//! We reuse riichienv-core's canonical action encoding so our labels line up
//! with its legal-action enumeration:
//! - 4-player: 82 ids (`Action::encode`) — 0..=33 discard tile34, 37 riichi,
//!   38/39/40 chi lo/mid/hi, 41 pon, 42..=75 kan (=42+tile34), 79 agari,
//!   80 kyushu, 81 pass.
//! - 3-player: 60 ids (`ActionEncoder::ThreePlayer`) — 0..=26 discard compact,
//!   27 riichi, 28 pon, 29..=55 kan, 56 agari, 57 kyushu, 58 pass, 59 kita.

use riichienv_core::action::{Action, ActionEncoder, ACTION_SPACE_3P, ACTION_SPACE_4P};

/// Size of the discrete action space for the given player count.
pub fn action_space(num_players: u8) -> usize {
    if num_players == 3 {
        ACTION_SPACE_3P
    } else {
        ACTION_SPACE_4P
    }
}

/// Action id of `Pass` for the given player count (81 for 4p, 58 for 3p).
pub fn pass_index(num_players: u8) -> usize {
    if num_players == 3 {
        58
    } else {
        81
    }
}

/// Index of an action in the mode-appropriate action space, or `None` if the
/// action is invalid for that mode (e.g. chi in sanma).
pub fn action_index(action: &Action, num_players: u8) -> Option<usize> {
    ActionEncoder::from_num_players(num_players)
        .encode(action)
        .ok()
        .map(|i| i as usize)
}

/// Build a legal-action mask (`1` = legal) of length [`action_space`].
pub fn legal_mask(legal: &[Action], num_players: u8) -> Vec<u8> {
    let mut mask = vec![0u8; action_space(num_players)];
    for a in legal {
        if let Some(idx) = action_index(a, num_players) {
            mask[idx] = 1;
        }
    }
    mask
}

/// Choose the legal action whose logit is highest.
///
/// `logits` must be indexed by the mode's action space. Illegal indices are
/// never considered, so the returned action is always legal. Returns `None`
/// only if `legal` is empty.
pub fn pick_by_logits(legal: &[Action], logits: &[f32], num_players: u8) -> Option<Action> {
    let mut best: Option<(f32, &Action)> = None;
    for a in legal {
        if let Some(idx) = action_index(a, num_players) {
            let score = logits.get(idx).copied().unwrap_or(f32::NEG_INFINITY);
            match best {
                Some((b, _)) if score <= b => {}
                _ => best = Some((score, a)),
            }
        }
    }
    best.map(|(_, a)| a.clone())
}

/// Rank legal actions by logit (highest first) and return the top `top_n` as
/// `(action, prob)` pairs, where `prob` is the softmax of the logits taken
/// **over the legal actions only** (same normalization the remote API uses for
/// its candidate distribution). The first element matches [`pick_by_logits`].
pub fn rank_by_logits(
    legal: &[Action],
    logits: &[f32],
    num_players: u8,
    top_n: usize,
) -> Vec<(Action, f32)> {
    let mut scored: Vec<(&Action, f32)> = legal
        .iter()
        .filter_map(|a| {
            action_index(a, num_players)
                .map(|i| (a, logits.get(i).copied().unwrap_or(f32::NEG_INFINITY)))
        })
        .collect();
    if scored.is_empty() {
        return Vec::new();
    }
    let max_logit = scored
        .iter()
        .map(|(_, l)| *l)
        .fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = scored.iter().map(|(_, l)| (l - max_logit).exp()).collect();
    let sum: f32 = exps.iter().sum();
    // Stable sort by logit desc: ties keep input order, matching pick_by_logits.
    let mut order: Vec<usize> = (0..scored.len()).collect();
    order.sort_by(|&a, &b| {
        scored[b]
            .1
            .partial_cmp(&scored[a].1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
        .into_iter()
        .take(top_n)
        .map(|i| {
            let prob = if sum > 0.0 { exps[i] / sum } else { 0.0 };
            (scored[i].0.clone(), prob)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use riichienv_core::action::{Action, ActionType};

    #[test]
    fn discard_index_is_tile34_4p() {
        // discard 5s (tile id 88 -> tile34 22)
        let a = Action::new(ActionType::Discard, Some(88), vec![], Some(0));
        assert_eq!(action_index(&a, 4), Some(22));
    }

    #[test]
    fn riichi_pon_pass_indices_4p() {
        assert_eq!(
            action_index(&Action::new(ActionType::Riichi, None, vec![], Some(0)), 4),
            Some(37)
        );
        assert_eq!(
            action_index(&Action::new(ActionType::Pon, Some(0), vec![1, 2], Some(0)), 4),
            Some(41)
        );
        assert_eq!(
            action_index(&Action::new(ActionType::Pass, None, vec![], Some(0)), 4),
            Some(81)
        );
    }

    #[test]
    fn discard_index_compact_3p() {
        // discard 1p (tile id 36 -> tile34 9 -> compact 2)
        let a = Action::new(ActionType::Discard, Some(36), vec![], Some(0));
        assert_eq!(action_index(&a, 3), Some(2));
        // chi is invalid in sanma
        let chi = Action::new(ActionType::Chi, Some(0), vec![4, 8], Some(0));
        assert_eq!(action_index(&chi, 3), None);
    }

    #[test]
    fn mask_and_pick() {
        let legal = vec![
            Action::new(ActionType::Discard, Some(0), vec![], Some(0)), // idx 0
            Action::new(ActionType::Pon, Some(4), vec![5, 6], Some(0)), // idx 41
            Action::new(ActionType::Pass, None, vec![], Some(0)),       // idx 81
        ];
        let mask = legal_mask(&legal, 4);
        assert_eq!(mask.len(), 82);
        assert_eq!(mask[0], 1);
        assert_eq!(mask[41], 1);
        assert_eq!(mask[81], 1);
        assert_eq!(mask[1], 0);

        let mut logits = vec![0.0f32; 82];
        logits[41] = 5.0; // prefer pon
        let picked = pick_by_logits(&legal, &logits, 4).unwrap();
        assert_eq!(picked.action_type, ActionType::Pon);
    }

    #[test]
    fn rank_orders_by_logit_and_normalizes_over_legal() {
        let legal = vec![
            Action::new(ActionType::Discard, Some(0), vec![], Some(0)), // idx 0
            Action::new(ActionType::Pon, Some(4), vec![5, 6], Some(0)), // idx 41
            Action::new(ActionType::Pass, None, vec![], Some(0)),       // idx 81
        ];
        let mut logits = vec![0.0f32; 82];
        logits[0] = 1.0;
        logits[41] = 3.0; // highest → ranked first
        logits[81] = 2.0;

        let ranked = rank_by_logits(&legal, &logits, 4, 3);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0.action_type, ActionType::Pon);
        assert_eq!(ranked[1].0.action_type, ActionType::Pass);
        assert_eq!(ranked[2].0.action_type, ActionType::Discard);
        // Probs are a descending softmax that sums to ~1 over the legal set.
        assert!(ranked[0].1 > ranked[1].1 && ranked[1].1 > ranked[2].1);
        let sum: f32 = ranked.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-5, "probs should sum to 1, got {sum}");
        // The top of the ranking matches pick_by_logits.
        let picked = pick_by_logits(&legal, &logits, 4).unwrap();
        assert_eq!(picked.action_type, ranked[0].0.action_type);
    }

    #[test]
    fn rank_top_n_truncates() {
        let legal = vec![
            Action::new(ActionType::Discard, Some(0), vec![], Some(0)),
            Action::new(ActionType::Discard, Some(4), vec![], Some(0)),
            Action::new(ActionType::Discard, Some(8), vec![], Some(0)),
        ];
        let logits = vec![0.0f32; 82];
        assert_eq!(rank_by_logits(&legal, &logits, 4, 2).len(), 2);
        assert!(rank_by_logits(&[], &logits, 4, 3).is_empty());
    }
}
