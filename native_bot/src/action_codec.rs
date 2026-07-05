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
}
