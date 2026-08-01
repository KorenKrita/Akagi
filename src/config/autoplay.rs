use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoplayConfig {
    /// Master switch. When `false`, no `AutoplayManager` is spawned and bot
    /// responses are not converted into UI clicks.
    pub enabled: bool,
    /// Per-platform autoplay knobs. Only the matching platform's section is
    /// consulted at runtime; the others sit dormant.
    pub majsoul: MajsoulAutoplayConfig,
    /// Pre-click delay model (platform-agnostic). See `autoplay::delay`.
    pub delay: DelayModelConfig,
}

/// Shape of the base thinking-time distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelayDistribution {
    /// `uniform(pre_click_delay_min_ms, pre_click_delay_max_ms)` — the
    /// historical Akagi behaviour. Default.
    #[default]
    Uniform,
    /// Log-normal over seconds (`exp(N(mu, sigma))`), clamped to the
    /// `[pre_click_delay_min_ms, pre_click_delay_max_ms]` window scaled by
    /// 0.5x/4x so a fat tail can exceed the old bounds without running
    /// away. Human reaction times are generally log-normal-ish; the
    /// concrete parameters should come from calibration data.
    LogNormal,
}

/// Parameters of the built-in pre-click delay model (`autoplay::delay`).
///
/// Defaults are chosen to be **behaviour-equivalent** with the historical
/// fixed `uniform(min, max)` delay: additive rule bonuses default to 0 and
/// the obvious-decision cap is disabled. Turning the knobs is deliberate
/// opt-in until calibration data justifies different defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelayModelConfig {
    /// Base distribution shape.
    pub distribution: DelayDistribution,
    /// Log-normal mu, in ln(seconds). Only used for `LogNormal`.
    pub lognormal_mu: f64,
    /// Log-normal sigma. Only used for `LogNormal`.
    pub lognormal_sigma: f64,
    /// Extra target time when riichi can be declared this turn (a genuine
    /// decision), ms. 0 = off.
    pub riichi_extra_ms: u32,
    /// Extra target time when the action is a kan declaration, ms. 0 = off.
    pub kan_extra_ms: u32,
    /// When the top-two candidate probabilities are closer than this, the
    /// decision counts as "hard": `close_margin_extra_ms` is added and the
    /// budget layer may dip into the server's extra time pool.
    pub close_margin: f64,
    /// Extra target time for a hard decision, ms. 0 = off.
    pub close_margin_extra_ms: u32,
    /// When the top candidate's probability exceeds this, the decision
    /// counts as "obvious" and the target is capped at `obvious_max_ms`.
    pub obvious_top_prob: f64,
    /// Cap for obvious decisions, ms. 0 = cap disabled.
    pub obvious_max_ms: u32,
    /// Reserved headroom inside the server window for network RTT and
    /// scheduling jitter, ms. The soft cap is
    /// `time_fixed - safety_margin - click_overhead`.
    pub safety_margin_ms: u32,
    /// Fraction of the server's extra time pool (`time_add`) a single
    /// hard decision may spend when the model allows bank use.
    pub bank_use_fraction: f64,
    /// Absolute cap on extra-pool spend for a single decision, ms.
    pub bank_max_single_ms: u32,
    /// Static cap applied when no server budget is known (non-Majsoul
    /// platform, or before the first operation list), ms. 0 = no cap.
    pub no_budget_cap_ms: u32,
    /// Lua override (`autoplay::delay::script`). When enabled and the
    /// script file exists, its `decide_delay(ctx)` replaces the built-in
    /// policy; any script failure falls back to the built-in model. The
    /// file being absent is the normal no-script state, not an error.
    pub script_enabled: bool,
    /// Path to the delay script. `None` = `<config dir>/scripts/delay.lua`.
    pub script_path: Option<String>,
}

impl Default for DelayModelConfig {
    fn default() -> Self {
        Self {
            distribution: DelayDistribution::Uniform,
            // Median ~1.8s, mildly fat tail — a placeholder pending
            // calibration; unused while `distribution` is Uniform.
            lognormal_mu: 0.6,
            lognormal_sigma: 0.5,
            riichi_extra_ms: 0,
            kan_extra_ms: 0,
            close_margin: 0.005,
            close_margin_extra_ms: 0,
            obvious_top_prob: 0.995,
            obvious_max_ms: 0,
            safety_margin_ms: 1000,
            bank_use_fraction: 0.25,
            bank_max_single_ms: 5000,
            // 15s static ceiling: far above anything the default
            // distribution produces, low enough to survive even a 5s+20
            // room's base window if the budget is somehow unknown.
            no_budget_cap_ms: 15_000,
            script_enabled: true,
            script_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MajsoulAutoplayConfig {
    /// Lower bound of the random pre-click delay (ms). The reference
    /// Akagi autoplay used `random.uniform(1.0, 3.0)` seconds; the same
    /// distribution is replicated here as `[1000, 3000]` ms by default.
    pub pre_click_delay_min_ms: u32,
    /// Upper bound of the random pre-click delay (ms).
    pub pre_click_delay_max_ms: u32,
    /// Inter-click delay between staged clicks within one action (e.g.
    /// reach button → riichi tile, or chi button → candidate select).
    pub inter_click_delay_ms: u32,
    /// How long to hover the mouse over a target before pressing.
    /// Empirically Laya's input system samples hover state before a
    /// mousedown registers a hit on the tile sprite — clicks issued
    /// without a hover delay (or shorter than ~100ms) get dropped on
    /// the floor. Default 150ms; do not lower below 100ms.
    pub hover_delay_ms: u32,
    /// How long to hold the mouse button down between mousePressed and
    /// mouseReleased. Non-zero so the engine doesn't collapse the pair
    /// into a single frame.
    pub click_hold_ms: u32,
    /// Extra delay tacked onto the dealer's first discard. Mahjong Soul
    /// plays a hand-sort animation when the dealer receives all 14 tiles
    /// at once; clicks issued during the animation are dropped. ~2s
    /// covers the animation across normal device speeds. Set to 0 to
    /// opt out (e.g. on a fast box where the animation finishes inside
    /// the regular pre-click delay anyway).
    pub dealer_first_discard_extra_delay_ms: u32,
}

impl Default for MajsoulAutoplayConfig {
    fn default() -> Self {
        Self {
            pre_click_delay_min_ms: 1000,
            pre_click_delay_max_ms: 3000,
            inter_click_delay_ms: 300,
            hover_delay_ms: 150,
            click_hold_ms: 50,
            dealer_first_discard_extra_delay_ms: 2000,
        }
    }
}
