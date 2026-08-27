//! Copilot-style weighted discard sampling for the built-in bot.
//!
//! Faithful port of MahjongCopilot's `automation.randomize_action`
//! (`game/automation.py`): when the model's top action is a discard, the
//! autoplay may play a **different** tile from the model's top-3 discards,
//! drawn according to the model's own policy probabilities.
//!
//! `level` semantics (mirrors Copilot's `ai_randomize_choice`, 0–5):
//! - `0` — off: always play the model's top pick (Akagi's historical
//!   behaviour, `argmax`).
//! - `1..=5` — on: sample from the top-3 discards weighted by
//!   `softmax_prob ** power` with `power = 1 / (0.2 * level)`.
//!   `level = 1` ⇒ `power = 5` (heavily concentrated on the top tile);
//!   `level = 5` ⇒ `power = 1` (raw policy probabilities).
//!
//! The crate deliberately avoids a `rand` dependency: a small self-contained
//! xorshift64* generator is plenty for this purpose and keeps the build lean.

/// Selection parameters for one engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionParams {
    /// 0 = off (argmax). 1..=5 = weighted sampling level.
    pub randomize_level: u8,
}

impl Default for SelectionParams {
    fn default() -> Self {
        Self { randomize_level: 0 }
    }
}

/// Clamp a user-supplied level into the supported 1..=5 range.
fn clamp_level(level: u8) -> u8 {
    level.clamp(1, 5)
}

/// Tiny deterministic RNG (xorshift64*). Seeded once per engine from the
/// wall clock so repeated games don't replay identical sequences.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform float in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Pick an index into `cands` (`(tile_id, prob)` pairs, best first) using
/// Copilot's power-scaled weighted sampling. Returns `None` when disabled,
/// empty, or when there is nothing to choose between.
///
/// `level` is clamped to `1..=5`; `0` always returns `None`.
pub fn pick_index(cands: &[(u8, f32)], level: u8, rng: &mut Rng) -> Option<usize> {
    if level == 0 || cands.len() < 2 {
        return None;
    }
    let power = 1.0 / (0.2 * f64::from(clamp_level(level)));
    let ws: Vec<f64> = cands
        .iter()
        .map(|&(_, p)| (f64::from(p)).powf(power))
        .collect();
    let sum: f64 = ws.iter().sum();
    if sum <= 0.0 {
        return None;
    }
    let mut r = rng.next_f64() * sum;
    for (i, w) in ws.iter().enumerate() {
        r -= w;
        if r <= 0.0 {
            return Some(i);
        }
    }
    Some(cands.len() - 1) // floating-point fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same seed must reproduce the same draw sequence.
    #[test]
    fn rng_is_deterministic_for_a_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_f64(), b.next_f64());
        }
    }

    /// level 0 / a single candidate never randomizes.
    #[test]
    fn disabled_or_singleton_returns_none() {
        let mut rng = Rng::new(1);
        let cands = [(0u8, 1.0f32), (1, 0.0), (2, 0.0)];
        assert_eq!(pick_index(&cands, 0, &mut rng), None);
        assert_eq!(pick_index(&[(0u8, 1.0f32)], 3, &mut rng), None);
    }

    /// level 1 (power 5) must almost always pick the top tile; level 5
    /// (power 1) still favors it but lets the tail show through.
    #[test]
    fn level_controls_concentration() {
        // A moderately close call: 1m 60%, 2p 30%, 西 10%.
        let cands = [(0u8, 0.6f32), (1, 0.3), (2, 0.1)];
        for (level, min_top, max_top) in [(1u8, 0.90, 1.00), (2, 0.60, 0.98), (5, 0.45, 0.90)] {
            let mut rng = Rng::new(7);
            let mut top = 0usize;
            let n = 10_000;
            for _ in 0..n {
                if pick_index(&cands, level, &mut rng) == Some(0) {
                    top += 1;
                }
            }
            let frac = top as f64 / n as f64;
            assert!(
                (min_top..=max_top).contains(&frac),
                "level {level}: top-tile fraction {frac:.3} outside {min_top}..={max_top}"
            );
        }
    }
}
