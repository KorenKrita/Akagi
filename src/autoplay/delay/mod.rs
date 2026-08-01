//! Pre-click delay model: how long a decision should appear to take.
//!
//! The model computes a **target total thinking time** for the current
//! decision window — the interval the server observes between opening the
//! window and receiving our action. That interval already contains network
//! latency, proxy overhead and bot inference, so the caller converts the
//! target into a sleep with `target - elapsed_since_window_opened` (see
//! `majsoul::push_pre_delay`), not by sleeping the target verbatim.
//!
//! Layering:
//! - [`decide`] — the pure built-in model. Base distribution (uniform by
//!   default, log-normal opt-in) plus additive bonuses for decision types
//!   that genuinely take a human longer (riichi declarable, kan, top-two
//!   candidates nearly tied) and a cap for obvious decisions. Defaults are
//!   behaviour-equivalent with the historical fixed `uniform(min, max)`.
//! - Budget enforcement (soft/hard caps from the server-granted
//!   [`TimeBudget`](crate::autoplay::budget::TimeBudget)) sits on top —
//!   the sampled target is clamped so autoplay can never run the window
//!   into an auto-discard.
//! - A user Lua script can replace [`decide`]'s policy; functional floors
//!   and the budget hard cap are enforced *after* the script so a
//!   misbehaving script cannot click into an animation or overrun the
//!   server clock.
//!
//! All quantities are milliseconds unless stated otherwise.

pub mod probs;

pub use probs::DecisionProbs;

use crate::config::{DelayDistribution, DelayModelConfig, MajsoulAutoplayConfig};
use rand::Rng;

/// What kind of action the bot chose. Derived from the mjai event; used
/// to pick delay characteristics, never to alter the action itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    Dahai,
    Reach,
    Chi,
    Pon,
    Daiminkan,
    Ankan,
    Kakan,
    Hora,
    Ryukyoku,
    Kita,
    /// Declining a claim window (mjai `none` with a visible Skip button).
    Pass,
}

impl DecisionKind {
    pub fn is_kan(self) -> bool {
        matches!(self, Self::Daiminkan | Self::Ankan | Self::Kakan)
    }
}

/// Copy of the server time budget taken at planning time.
/// `elapsed_ms` is pre-computed so the planner stays free of clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSnapshot {
    /// Base window time (`operation.time_fixed`).
    pub fixed_ms: u32,
    /// Extra time pool (`operation.time_add`).
    pub add_ms: u32,
    /// Already consumed since the window opened.
    pub elapsed_ms: u32,
}

/// Everything the model may consider. All fields are derivable from data
/// the autoplay manager already holds; none of them can alter the action.
#[derive(Debug, Clone)]
pub struct DelayInput<'a> {
    pub kind: DecisionKind,
    /// No tile in any kawa yet — the very first action of a kyoku.
    pub first_action_of_kyoku: bool,
    /// The click must wait out a dealing/sorting animation (dealer's
    /// 14-tile opening hand, or a kita on the opening draw). The
    /// animation wait is a functional floor: scripts cannot lower it.
    pub opening_animation: bool,
    /// Riichi is declarable this turn (`ActionType::Riichi` legal).
    pub can_riichi: bool,
    /// We are in accepted riichi (only Skip windows reach the planner).
    pub in_riichi: bool,
    pub legal_action_count: usize,
    /// Normalized top/second candidate probabilities, if the bot's meta
    /// could be interpreted. See [`probs::normalize_meta`].
    pub probs: Option<DecisionProbs>,
    /// Server time budget for this window, if known.
    pub budget: Option<BudgetSnapshot>,
    /// Time the click sequence itself will take *after* the pre-delay
    /// (hover/hold, candidate clicks, inter-click gaps). Deducted from
    /// the budget caps so multi-stage actions don't overrun.
    pub click_overhead_ms: u32,
    pub cfg: &'a MajsoulAutoplayConfig,
    pub delay_cfg: &'a DelayModelConfig,
}

/// Model output. `total_target_ms` is the target **total** thinking time
/// since the window opened — the caller subtracts what has already
/// elapsed. `allow_bank` reports whether this decision justifies dipping
/// into the server's extra time pool (`time_add`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayDecision {
    pub total_target_ms: u32,
    pub allow_bank: bool,
}

/// The built-in delay policy. Pure given `rng`.
pub fn decide<R: Rng + ?Sized>(input: &DelayInput, rng: &mut R) -> DelayDecision {
    let d = input.delay_cfg;

    let mut target = base_sample(input, rng);
    let mut allow_bank = false;

    if input.opening_animation {
        // Historical behaviour: the animation wait is added on top of the
        // random delay, keeping the visible post-animation pause random.
        target = target.saturating_add(input.cfg.dealer_first_discard_extra_delay_ms);
    }
    if input.can_riichi {
        target = target.saturating_add(d.riichi_extra_ms);
    }
    if input.kind.is_kan() {
        target = target.saturating_add(d.kan_extra_ms);
    }
    if let Some(p) = input.probs {
        if let Some(margin) = p.margin() {
            if margin < d.close_margin {
                // Genuinely hard call — think longer, and the budget layer
                // may spend extra time pool on it.
                target = target.saturating_add(d.close_margin_extra_ms);
                allow_bank = true;
            }
        }
        if d.obvious_max_ms > 0 && p.top > d.obvious_top_prob {
            target = target.min(d.obvious_max_ms);
        }
    }

    // Budget enforcement, then the functional floor. The floor is applied
    // last: clicking into the dealing animation loses the click entirely,
    // which is strictly worse than shaving budget headroom (and in
    // practice the base window is minutes, the floor a few seconds).
    target = target.min(budget_cap(input, allow_bank));
    target = target.max(functional_floor(input));

    DelayDecision {
        total_target_ms: target,
        allow_bank,
    }
}

/// The hard ceiling for the target total thinking time.
///
/// With a known server budget:
///
/// ```text
/// soft_cap = time_fixed - safety_margin - click_overhead
/// hard_cap = soft_cap + min(time_add * bank_use_fraction, bank_max_single)
/// ```
///
/// The soft cap never touches the extra time pool; only a decision the
/// model marked `allow_bank` (top-two near-tie) may reach the hard cap.
/// With `time_add == 0` the two coincide. Exceeding the hard cap would
/// mean the client auto-discards for us — the most conspicuous failure
/// mode there is — so the cap is unconditional.
///
/// Without a budget, a static configurable ceiling applies.
pub fn budget_cap(input: &DelayInput, allow_bank: bool) -> u32 {
    let d = input.delay_cfg;
    match input.budget {
        Some(b) => {
            let soft = b
                .fixed_ms
                .saturating_sub(d.safety_margin_ms)
                .saturating_sub(input.click_overhead_ms);
            if allow_bank {
                let bank = (f64::from(b.add_ms) * d.bank_use_fraction.clamp(0.0, 1.0)) as u32;
                soft.saturating_add(bank.min(d.bank_max_single_ms))
            } else {
                soft
            }
        }
        None => {
            if d.no_budget_cap_ms == 0 {
                u32::MAX
            } else {
                d.no_budget_cap_ms
            }
        }
    }
}

/// The minimum target no policy — built-in or script — may go below.
pub fn functional_floor(input: &DelayInput) -> u32 {
    if input.opening_animation {
        input.cfg.dealer_first_discard_extra_delay_ms
    } else {
        0
    }
}

/// Sample the base thinking time.
fn base_sample<R: Rng + ?Sized>(input: &DelayInput, rng: &mut R) -> u32 {
    let lo = input.cfg.pre_click_delay_min_ms;
    let hi = input.cfg.pre_click_delay_max_ms.max(lo);

    // Reference behaviour (`autoplay_majsoul.py:156-157`): the first
    // action of a kyoku uses the upper bound as a fixed delay — slightly
    // slower but more human on the opening turn.
    if input.first_action_of_kyoku {
        return hi;
    }

    match input.delay_cfg.distribution {
        DelayDistribution::Uniform => {
            if hi == lo {
                lo
            } else {
                rng.random_range(lo..=hi)
            }
        }
        DelayDistribution::LogNormal => {
            let d = input.delay_cfg;
            match rand_distr::LogNormal::new(d.lognormal_mu, d.lognormal_sigma) {
                Ok(dist) => {
                    let secs: f64 = rng.sample(dist);
                    let ms = (secs * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32;
                    // Loose clamp: allow the tail to exceed the uniform
                    // window (that's the point of a fat tail) but keep it
                    // bounded, and never go implausibly fast.
                    ms.clamp(lo / 2, hi.saturating_mul(4))
                }
                // Invalid sigma from a hand-edited config — fall back to
                // the uniform behaviour rather than panicking mid-game.
                Err(_) => rng.random_range(lo..=hi.max(lo + 1)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn cfg() -> MajsoulAutoplayConfig {
        MajsoulAutoplayConfig::default()
    }

    fn input<'a>(
        cfg: &'a MajsoulAutoplayConfig,
        delay_cfg: &'a DelayModelConfig,
    ) -> DelayInput<'a> {
        DelayInput {
            kind: DecisionKind::Dahai,
            first_action_of_kyoku: false,
            opening_animation: false,
            can_riichi: false,
            in_riichi: false,
            legal_action_count: 1,
            probs: None,
            budget: None,
            click_overhead_ms: 0,
            cfg,
            delay_cfg,
        }
    }

    const AKAGI_SEED: u64 = 0xA4A61;

    /// Default config must reproduce the historical behaviour: a uniform
    /// draw inside [min, max].
    #[test]
    fn default_model_matches_legacy_uniform() {
        let c = cfg();
        let d = DelayModelConfig::default();
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        for _ in 0..1000 {
            let dec = decide(&input(&c, &d), &mut r);
            assert!(
                (c.pre_click_delay_min_ms..=c.pre_click_delay_max_ms)
                    .contains(&dec.total_target_ms),
                "target {} outside legacy window",
                dec.total_target_ms
            );
            assert!(!dec.allow_bank);
        }
    }

    /// First action of a kyoku pins the base to the upper bound (legacy).
    #[test]
    fn first_action_uses_upper_bound() {
        let c = cfg();
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.first_action_of_kyoku = true;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(dec.total_target_ms, c.pre_click_delay_max_ms);
    }

    /// Dealer-opening fold-in: animation wait is added on top of the base
    /// and acts as a floor (regression for the folded
    /// `dealer_first_discard_extra_delay_ms` sleep).
    #[test]
    fn opening_animation_adds_and_floors() {
        let c = cfg();
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.first_action_of_kyoku = true;
        i.opening_animation = true;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(
            dec.total_target_ms,
            c.pre_click_delay_max_ms + c.dealer_first_discard_extra_delay_ms
        );
        assert!(dec.total_target_ms >= functional_floor(&i));
    }

    /// Rule bonuses are additive and off by default.
    #[test]
    fn rule_bonuses_apply_when_configured() {
        let c = cfg();
        let mut d = DelayModelConfig::default();
        d.riichi_extra_ms = 2000;
        d.kan_extra_ms = 500;
        let mut i = input(&c, &d);
        i.kind = DecisionKind::Ankan;
        i.can_riichi = true;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(
            dec.total_target_ms >= c.pre_click_delay_min_ms + 2500,
            "bonuses must be added (got {})",
            dec.total_target_ms
        );
    }

    /// A near-tie between the top two candidates marks the decision as
    /// hard: extra time + permission to use the bank.
    #[test]
    fn close_margin_marks_hard_decision() {
        let c = cfg();
        let mut d = DelayModelConfig::default();
        d.close_margin_extra_ms = 2000;
        let mut i = input(&c, &d);
        i.probs = Some(DecisionProbs {
            top: 0.41,
            second: Some(0.409),
        });
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(dec.allow_bank, "near-tie must allow bank");
        assert!(dec.total_target_ms >= c.pre_click_delay_min_ms + 2000);
    }

    /// An obvious decision (top prob ~1) is capped when the cap is enabled,
    /// and uncapped by default.
    #[test]
    fn obvious_decision_cap() {
        let c = cfg();
        let probs = Some(DecisionProbs {
            top: 0.999,
            second: Some(0.001),
        });

        // Disabled by default: behaves like legacy.
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.probs = probs;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(dec.total_target_ms >= c.pre_click_delay_min_ms);

        let mut capped = DelayModelConfig::default();
        capped.obvious_max_ms = 800;
        let mut i = input(&c, &capped);
        i.probs = probs;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(dec.total_target_ms <= 800);
    }

    // ------------------------------------------------------------------
    // Budget enforcement (soft/hard caps)
    // ------------------------------------------------------------------

    fn tight_budget_cfg() -> MajsoulAutoplayConfig {
        // Force the sampled target far above the caps so the cap is what
        // decides the outcome.
        let mut c = cfg();
        c.pre_click_delay_min_ms = 100_000;
        c.pre_click_delay_max_ms = 100_000;
        c
    }

    /// With `time_add == 0` the target never exceeds the soft cap
    /// (`fixed - safety_margin - click_overhead`).
    #[test]
    fn soft_cap_binds_without_bank() {
        let c = tight_budget_cfg();
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 0,
            elapsed_ms: 0,
        });
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(dec.total_target_ms, 5000 - d.safety_margin_ms);
    }

    /// Click-sequence overhead (hover/hold, candidate clicks, riichi
    /// two-step) is reserved out of the window — multi-stage actions must
    /// not systematically overrun.
    #[test]
    fn click_overhead_reserved_from_cap() {
        let c = tight_budget_cfg();
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 0,
            elapsed_ms: 0,
        });
        i.click_overhead_ms = 700;
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(dec.total_target_ms, 5000 - d.safety_margin_ms - 700);
    }

    /// Only an `allow_bank` decision (top-two near-tie) may spend the
    /// extra pool, bounded by fraction and absolute single-spend cap —
    /// and never beyond the hard cap.
    #[test]
    fn hard_cap_requires_bank_permission() {
        let c = tight_budget_cfg();
        let mut d = DelayModelConfig::default();
        d.close_margin_extra_ms = 1; // near-tie marks allow_bank
        let budget = BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 20_000,
            elapsed_ms: 0,
        };

        // Not a near-tie: soft cap despite a fat bank.
        let mut i = input(&c, &d);
        i.budget = Some(budget);
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(!dec.allow_bank);
        assert_eq!(dec.total_target_ms, 5000 - d.safety_margin_ms);

        // Near-tie: hard cap = soft + min(20000 * 0.25, bank_max_single).
        let mut i = input(&c, &d);
        i.budget = Some(budget);
        i.probs = Some(DecisionProbs {
            top: 0.5,
            second: Some(0.499),
        });
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(dec.allow_bank);
        let expected_bank = 5000u32.min(d.bank_max_single_ms);
        assert_eq!(
            dec.total_target_ms,
            5000 - d.safety_margin_ms + expected_bank
        );
    }

    /// No budget known: the static ceiling applies (and can be disabled).
    #[test]
    fn static_cap_without_budget() {
        let c = tight_budget_cfg();
        let d = DelayModelConfig::default();
        let i = input(&c, &d);
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(dec.total_target_ms, d.no_budget_cap_ms);

        let mut open = DelayModelConfig::default();
        open.no_budget_cap_ms = 0;
        let i = input(&c, &open);
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert_eq!(dec.total_target_ms, 100_000, "0 disables the static cap");
    }

    /// The functional floor (animation wait) wins over the cap: losing
    /// the click to the animation is worse than shaving headroom.
    #[test]
    fn functional_floor_beats_cap() {
        let c = cfg(); // dealer pad 2000ms default
        let d = DelayModelConfig::default();
        let mut i = input(&c, &d);
        i.opening_animation = true;
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 1500, // window smaller than the animation
            add_ms: 0,
            elapsed_ms: 0,
        });
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let dec = decide(&i, &mut r);
        assert!(
            dec.total_target_ms >= c.dealer_first_discard_extra_delay_ms,
            "floor must not be undercut by the cap"
        );
    }

    /// Distribution sanity: fixed seed, log-normal median lands near
    /// exp(mu) seconds and stays inside the loose clamp.
    #[test]
    fn lognormal_distribution_sanity() {
        let c = cfg();
        let mut d = DelayModelConfig::default();
        d.distribution = DelayDistribution::LogNormal;
        d.lognormal_mu = 0.6; // e^0.6 ≈ 1.82s
        d.lognormal_sigma = 0.5;
        let i = input(&c, &d);
        let mut r = StdRng::seed_from_u64(AKAGI_SEED);
        let mut samples: Vec<u32> = (0..10_000)
            .map(|_| decide(&i, &mut r).total_target_ms)
            .collect();
        samples.sort_unstable();
        let med = samples[samples.len() / 2];
        assert!(
            (1500..=2200).contains(&med),
            "log-normal median {med} out of tolerance"
        );
        let lo = c.pre_click_delay_min_ms / 2;
        let hi = c.pre_click_delay_max_ms * 4;
        assert!(*samples.first().unwrap() >= lo);
        assert!(*samples.last().unwrap() <= hi);
    }
}
