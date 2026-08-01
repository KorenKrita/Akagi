//! Lua override for the pre-click delay policy.
//!
//! Users drop a `delay.lua` next to their config (or point
//! `autoplay.delay.script_path` anywhere) defining:
//!
//! ```lua
//! function decide_delay(ctx)
//!   return { delay_ms = 2300, allow_bank = false }
//! end
//! ```
//!
//! `delay_ms` is the target **total** thinking time for the decision
//! window (the interval the server observes), not a sleep length — the
//! caller subtracts time already consumed.
//!
//! Safety contract (all enforced here, not trusted to the script):
//! - Restricted stdlib (math/string/table only — no io/os).
//! - Instruction budget + wall-clock deadline via a VM hook.
//! - Any failure (missing file is not a failure; syntax error, runtime
//!   error, timeout, wrong return shape, out-of-range delay) falls back
//!   to the built-in model and is logged **once** per distinct error,
//!   not once per hand.
//! - The result is clamped by [`budget_cap`] and [`functional_floor`] —
//!   a script can neither overrun the server clock nor click into a
//!   dealing animation.
//! - The script never sees or influences the chosen action.

use super::{budget_cap, functional_floor, DelayDecision, DelayInput};
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value, VmState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

/// The default delay policy, generated as `delay.lua` next to the
/// config file on first use (see [`ScriptHost::maybe_reload`]).
pub const DEFAULT_SCRIPT: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/delay_default.lua"));

/// Hard sanity range for a script-provided target (10 minutes). Values
/// outside are treated as a script bug, not clamped silently.
const MAX_SCRIPT_DELAY_MS: f64 = 600_000.0;
/// Wall-clock deadline for one script call.
const CALL_DEADLINE: Duration = Duration::from_millis(50);
/// The VM hook fires every N instructions to check the deadline.
const HOOK_EVERY_N_INSTRUCTIONS: u32 = 1_000;
/// Instruction budget for one call (hook fires × N).
const MAX_HOOK_FIRES: u64 = 10_000; // = 10M instructions

/// A loaded, compiled delay script.
pub struct DelayScript {
    lua: Lua,
    func: Function,
    /// Last error reported for this script instance; used to log each
    /// distinct failure once instead of spamming every hand. Mutex (not
    /// RefCell) because `&DelayScript` is held inside `ActionContext`
    /// across an await, which requires `Sync`; it is never contended.
    last_error: std::sync::Mutex<Option<String>>,
}

impl DelayScript {
    /// Compile `source` and resolve the `decide_delay` global.
    pub fn compile(source: &str, chunk_name: &str) -> Result<Self, String> {
        let lua = Lua::new_with(
            StdLib::MATH | StdLib::STRING | StdLib::TABLE,
            LuaOptions::default(),
        )
        .map_err(|e| format!("lua init: {e}"))?;
        lua.load(source)
            .set_name(chunk_name)
            .exec()
            .map_err(|e| format!("script load: {e}"))?;
        let func: Function = lua
            .globals()
            .get("decide_delay")
            .map_err(|_| "script defines no `decide_delay` function".to_string())?;
        Ok(Self {
            lua,
            func,
            last_error: std::sync::Mutex::new(None),
        })
    }

    /// Run the script for one decision. `None` means "fall back to the
    /// built-in model" (and the cause has been logged once).
    pub fn try_decide(&self, input: &DelayInput) -> Option<DelayDecision> {
        let deadline = Instant::now() + CALL_DEADLINE;
        let fires = Arc::new(AtomicU64::new(0));
        let fires_hook = fires.clone();
        let hook_installed = self.lua.set_hook(
            mlua::HookTriggers::new().every_nth_instruction(HOOK_EVERY_N_INSTRUCTIONS),
            move |_lua, _debug| {
                if fires_hook.fetch_add(1, Ordering::Relaxed) >= MAX_HOOK_FIRES {
                    return Err(mlua::Error::RuntimeError(
                        "delay script exceeded instruction budget".into(),
                    ));
                }
                if Instant::now() > deadline {
                    return Err(mlua::Error::RuntimeError(
                        "delay script exceeded time budget".into(),
                    ));
                }
                Ok(VmState::Continue)
            },
        );
        if hook_installed.is_err() {
            // No hook means no runaway protection — refuse to run the
            // script rather than risk blocking the autoplay loop.
            warn!("delay script: could not install VM hook — using built-in model");
            return None;
        }
        let result = self.call(input);
        self.lua.remove_hook();

        match result {
            Ok((target_ms, allow_bank)) => {
                if let Ok(mut g) = self.last_error.lock() {
                    *g = None;
                }
                // Enforce the non-negotiables on the script's answer.
                let target = target_ms
                    .min(budget_cap(input, allow_bank))
                    .max(functional_floor(input));
                Some(DelayDecision {
                    total_target_ms: target,
                    allow_bank,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                if let Ok(mut g) = self.last_error.lock() {
                    if g.as_deref() != Some(msg.as_str()) {
                        warn!("delay script failed — using built-in model: {msg}");
                        *g = Some(msg);
                    }
                }
                None
            }
        }
    }

    fn call(&self, input: &DelayInput) -> mlua::Result<(u32, bool)> {
        let ctx = self.build_ctx(input)?;
        let ret: Value = self.func.call(ctx)?;
        let Value::Table(t) = ret else {
            return Err(mlua::Error::RuntimeError(
                "decide_delay must return a table { delay_ms, allow_bank }".into(),
            ));
        };
        let delay_ms: f64 = t.get("delay_ms").map_err(|_| {
            mlua::Error::RuntimeError("decide_delay result is missing numeric `delay_ms`".into())
        })?;
        if !delay_ms.is_finite() || !(0.0..=MAX_SCRIPT_DELAY_MS).contains(&delay_ms) {
            return Err(mlua::Error::RuntimeError(format!(
                "decide_delay returned out-of-range delay_ms {delay_ms}"
            )));
        }
        let allow_bank = t
            .get::<Option<bool>>("allow_bank")
            .ok()
            .flatten()
            .unwrap_or(false);
        Ok((delay_ms as u32, allow_bank))
    }

    /// Build the read-only `ctx` table the script receives.
    fn build_ctx(&self, input: &DelayInput) -> mlua::Result<Table> {
        let lua = &self.lua;
        let ctx = lua.create_table()?;
        ctx.set("action", kind_name(input.kind))?;
        ctx.set("tsumogiri", input.is_tsumogiri)?;
        ctx.set("post_call", input.is_post_call)?;
        ctx.set("first_action", input.first_action_of_kyoku)?;
        ctx.set("dealer_opening", input.opening_animation)?;
        ctx.set("can_riichi", input.can_riichi)?;
        ctx.set("is_kan", input.kind.is_kan())?;
        ctx.set("in_riichi", input.in_riichi)?;
        ctx.set("opponent_riichi", input.opponent_riichi)?;
        if let Some(tc) = input.tile_class {
            ctx.set("tile_class", tc.as_str())?;
        }
        ctx.set("junme", input.junme)?;
        ctx.set("legal_count", input.legal_action_count)?;
        if let Some(p) = input.probs {
            ctx.set("top_prob", p.top)?;
            if let Some(second) = p.second {
                ctx.set("second_prob", second)?;
            }
            if let Some(margin) = p.margin() {
                ctx.set("margin", margin)?;
            }
        }
        if let Some(b) = input.budget {
            let budget = lua.create_table()?;
            budget.set("fixed_ms", b.fixed_ms)?;
            budget.set("add_ms", b.add_ms)?;
            budget.set("elapsed_ms", b.elapsed_ms)?;
            ctx.set("budget", budget)?;
        }
        ctx.set(
            "rng",
            lua.create_function(|_, ()| Ok(rand::random::<f64>()))?,
        )?;
        ctx.set(
            "lognormal",
            lua.create_function(|_, (mu, sigma): (f64, f64)| {
                match rand_distr::LogNormal::new(mu, sigma) {
                    Ok(dist) => {
                        use rand::Rng;
                        Ok(rand::rng().sample::<f64, _>(dist))
                    }
                    Err(_) => Err(mlua::Error::RuntimeError(
                        "lognormal: invalid sigma".into(),
                    )),
                }
            })?,
        )?;
        Ok(ctx)
    }
}

fn kind_name(kind: super::DecisionKind) -> &'static str {
    use super::DecisionKind::*;
    match kind {
        Dahai => "dahai",
        Reach => "reach",
        Chi => "chi",
        Pon => "pon",
        Daiminkan => "daiminkan",
        Ankan => "ankan",
        Kakan => "kakan",
        Hora => "hora",
        Ryukyoku => "ryukyoku",
        Kita => "kita",
        Pass => "none",
    }
}

/// Owns the optional script and its hot-reload state. The autoplay
/// manager calls [`ScriptHost::maybe_reload`] (cheap mtime stat) before
/// planning; the platform planner calls [`ScriptHost::script`].
#[derive(Default)]
pub struct ScriptHost {
    script: Option<DelayScript>,
    /// mtime of the last load *attempt* (successful or not) — a broken
    /// file is not recompiled until it changes.
    attempted_mtime: Option<SystemTime>,
    attempted_path: Option<PathBuf>,
    /// Whether the last attempt failed (logged already).
    load_failed: bool,
    /// Default-file generation failed (read-only fs) — logged once,
    /// not retried every hand.
    generate_failed: bool,
}

impl ScriptHost {
    /// Write [`DEFAULT_SCRIPT`] to `path` if no file exists there yet.
    /// A deleted file is regenerated on the next start; a failure (e.g.
    /// read-only install) is logged once and the built-in model runs.
    pub fn ensure_default(&mut self, path: &Path) {
        if self.generate_failed || path.exists() {
            return;
        }
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(path, DEFAULT_SCRIPT)
        };
        match write() {
            Ok(()) => info!("generated default delay script: {}", path.display()),
            Err(e) => {
                warn!(
                    "could not generate delay script at {} — using built-in model: {e}",
                    path.display()
                );
                self.generate_failed = true;
            }
        }
    }

    /// (Re)load the script when the file at `path` appeared, changed or
    /// vanished. A missing file is the normal no-script state.
    pub fn maybe_reload(&mut self, path: &Path, enabled: bool) {
        if !enabled {
            self.script = None;
            self.attempted_mtime = None;
            self.attempted_path = None;
            return;
        }
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let path_changed = self.attempted_path.as_deref() != Some(path);
        if !path_changed && mtime == self.attempted_mtime {
            return; // nothing new — keep current state (script or None)
        }
        self.attempted_path = Some(path.to_path_buf());
        self.attempted_mtime = mtime;
        self.load_failed = false;

        let Some(_) = mtime else {
            if self.script.is_some() {
                info!("delay script removed — using built-in model");
            }
            self.script = None;
            return;
        };
        match std::fs::read_to_string(path) {
            Ok(source) => match DelayScript::compile(&source, &path.display().to_string()) {
                Ok(script) => {
                    info!("delay script loaded: {}", path.display());
                    self.script = Some(script);
                }
                Err(e) => {
                    warn!("delay script rejected ({}): {e}", path.display());
                    self.script = None;
                    self.load_failed = true;
                }
            },
            Err(e) => {
                warn!("delay script unreadable ({}): {e}", path.display());
                self.script = None;
                self.load_failed = true;
            }
        }
    }

    pub fn script(&self) -> Option<&DelayScript> {
        self.script.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BudgetSnapshot, DecisionKind, DelayInput};
    use super::*;
    use crate::config::{DelayModelConfig, MajsoulAutoplayConfig};

    fn base_input<'a>(
        cfg: &'a MajsoulAutoplayConfig,
        delay_cfg: &'a DelayModelConfig,
    ) -> DelayInput<'a> {
        DelayInput {
            kind: DecisionKind::Dahai,
            is_tsumogiri: false,
            is_post_call: false,
            first_action_of_kyoku: false,
            opening_animation: false,
            can_riichi: false,
            in_riichi: false,
            opponent_riichi: false,
            tile_class: None,
            junme: 0,
            legal_action_count: 1,
            probs: None,
            budget: None,
            click_overhead_ms: 0,
            cfg,
            delay_cfg,
        }
    }

    #[test]
    fn script_result_is_used() {
        let s = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 2300 } end",
            "test",
        )
        .unwrap();
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();
        let dec = s.try_decide(&base_input(&cfg, &d)).unwrap();
        assert_eq!(dec.total_target_ms, 2300);
        assert!(!dec.allow_bank);
    }

    #[test]
    fn script_sees_ctx_and_helpers() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              assert(ctx.action == "dahai")
              assert(ctx.first_action == false)
              assert(ctx.legal_count == 1)
              assert(ctx.budget.fixed_ms == 5000)
              assert(ctx.budget.add_ms == 20000)
              local r = ctx.rng()
              assert(r >= 0 and r < 1)
              local ln = ctx.lognormal(0.6, 0.5)
              assert(ln > 0)
              return { delay_ms = 1000 + ctx.budget.elapsed_ms, allow_bank = true }
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();
        let mut i = base_input(&cfg, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 20_000,
            elapsed_ms: 250,
        });
        let dec = s.try_decide(&i).unwrap();
        assert_eq!(dec.total_target_ms, 1250);
        assert!(dec.allow_bank);
    }

    #[test]
    fn syntax_error_is_rejected_at_compile() {
        assert!(DelayScript::compile("function decide_delay(", "test").is_err());
        assert!(DelayScript::compile("x = 1", "test").is_err(), "no function");
    }

    #[test]
    fn runtime_error_falls_back() {
        let s = DelayScript::compile(
            "function decide_delay(ctx) error('boom') end",
            "test",
        )
        .unwrap();
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
    }

    #[test]
    fn wrong_return_shape_falls_back() {
        for src in [
            "function decide_delay(ctx) return 42 end",
            "function decide_delay(ctx) return { } end",
            "function decide_delay(ctx) return { delay_ms = 'soon' } end",
            "function decide_delay(ctx) return { delay_ms = -5 } end",
            "function decide_delay(ctx) return { delay_ms = 1e12 } end",
            "function decide_delay(ctx) return { delay_ms = 0/0 } end",
        ] {
            let s = DelayScript::compile(src, "test").unwrap();
            let cfg = MajsoulAutoplayConfig::default();
            let d = DelayModelConfig::default();
            assert!(
                s.try_decide(&base_input(&cfg, &d)).is_none(),
                "must fall back for: {src}"
            );
        }
    }

    #[test]
    fn infinite_loop_is_aborted() {
        let s = DelayScript::compile(
            "function decide_delay(ctx) while true do end end",
            "test",
        )
        .unwrap();
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();
        let start = Instant::now();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "hook must abort the loop quickly"
        );
    }

    /// The functional floor and budget cap bind the script's answer:
    /// returning 0 cannot click into the dealing animation, and a huge
    /// value cannot overrun the server window.
    #[test]
    fn script_cannot_break_floor_or_cap() {
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();

        let zero = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 0 } end",
            "test",
        )
        .unwrap();
        let mut i = base_input(&cfg, &d);
        i.opening_animation = true;
        let dec = zero.try_decide(&i).unwrap();
        assert_eq!(
            dec.total_target_ms, cfg.dealer_first_discard_extra_delay_ms,
            "animation floor must hold against a 0 return"
        );

        // Regression: even outside the opening animation, a 0 return is
        // lifted to the UI-readiness floor — clicking before Majsoul
        // renders the buttons loses the click.
        let dec = zero.try_decide(&base_input(&cfg, &d)).unwrap();
        assert_eq!(
            dec.total_target_ms, d.min_delay_ms,
            "min_delay_ms floor must hold against a 0 return"
        );

        let huge = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 500000 } end",
            "test",
        )
        .unwrap();
        let mut i = base_input(&cfg, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 0,
            elapsed_ms: 0,
        });
        let dec = huge.try_decide(&i).unwrap();
        assert_eq!(
            dec.total_target_ms,
            5000 - d.safety_margin_ms,
            "soft cap must hold against a huge return"
        );
    }

    /// The bundled default (`DEFAULT_SCRIPT`, generated as `delay.lua`)
    /// must compile and produce human-plausible values.
    #[test]
    fn bundled_default_script_works() {
        let s = DelayScript::compile(DEFAULT_SCRIPT, "delay_default.lua").unwrap();
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();

        // Plain tedashi: calibrated median ~2.4s. The script rng is not
        // seedable from here, so assert on a batch median with slack.
        let mut samples: Vec<u32> = (0..300)
            .map(|_| s.try_decide(&base_input(&cfg, &d)).unwrap().total_target_ms)
            .collect();
        samples.sort_unstable();
        let med = samples[samples.len() / 2];
        assert!(
            (1500..=4000).contains(&med),
            "default-script tedashi median {med} implausible"
        );
        // Floors/caps still bind: nothing below min_delay_ms, nothing
        // above the no-budget static cap.
        assert!(*samples.first().unwrap() >= d.min_delay_ms);
        assert!(*samples.last().unwrap() <= d.no_budget_cap_ms);

        // In riichi: measured Throne players still glance (~1.4s median),
        // never below the UI-readiness floor.
        let mut riichi_samples: Vec<u32> = (0..300)
            .map(|_| {
                let mut i = base_input(&cfg, &d);
                i.in_riichi = true;
                i.kind = DecisionKind::Pass;
                s.try_decide(&i).unwrap().total_target_ms
            })
            .collect();
        riichi_samples.sort_unstable();
        assert!(*riichi_samples.first().unwrap() >= d.min_delay_ms);
        let riichi_med = riichi_samples[riichi_samples.len() / 2];
        assert!(
            (1100..=1800).contains(&riichi_med),
            "in-riichi median {riichi_med} off calibration (~1.4s)"
        );

        // Near-tie -> long thought with bank allowed.
        let mut i = base_input(&cfg, &d);
        i.probs = Some(crate::autoplay::delay::DecisionProbs {
            top: 0.40,
            second: Some(0.399),
        });
        assert!(s.try_decide(&i).unwrap().allow_bank);

        // Claim windows are the fast reaction bucket: batch median well
        // under the tedashi one.
        let mut claims: Vec<u32> = (0..300)
            .map(|_| {
                let mut i = base_input(&cfg, &d);
                i.kind = DecisionKind::Pass;
                s.try_decide(&i).unwrap().total_target_ms
            })
            .collect();
        claims.sort_unstable();
        assert!(claims[claims.len() / 2] < med);
    }

    /// `ensure_default` generates the bundled script once and never
    /// overwrites user edits.
    #[test]
    fn ensure_default_generates_once_and_preserves_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delay.lua");
        let mut host = ScriptHost::default();

        host.ensure_default(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_SCRIPT,
            "missing file must be created from the bundled default"
        );

        std::fs::write(&path, "-- user edit\nfunction decide_delay(c) return { delay_ms = 1 } end")
            .unwrap();
        host.ensure_default(&path);
        assert!(
            std::fs::read_to_string(&path).unwrap().starts_with("-- user edit"),
            "an existing file must never be overwritten"
        );
    }

    #[test]
    fn host_hot_reloads_and_tolerates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delay.lua");
        let mut host = ScriptHost::default();

        // Missing file: normal no-script state.
        host.maybe_reload(&path, true);
        assert!(host.script().is_none());

        std::fs::write(
            &path,
            "function decide_delay(ctx) return { delay_ms = 1111 } end",
        )
        .unwrap();
        host.maybe_reload(&path, true);
        assert!(host.script().is_some());

        // Rewrite with a different value (bump mtime explicitly — some
        // filesystems have coarse timestamps).
        std::fs::write(
            &path,
            "function decide_delay(ctx) return { delay_ms = 2222 } end",
        )
        .unwrap();
        let bumped = SystemTime::now() + Duration::from_secs(2);
        let f = std::fs::File::open(&path).unwrap();
        f.set_modified(bumped).unwrap();
        host.maybe_reload(&path, true);
        let cfg = MajsoulAutoplayConfig::default();
        let d = DelayModelConfig::default();
        let dec = host
            .script()
            .unwrap()
            .try_decide(&base_input(&cfg, &d))
            .unwrap();
        assert_eq!(dec.total_target_ms, 2222, "reload must pick up the edit");

        // Disabled: script dropped.
        host.maybe_reload(&path, false);
        assert!(host.script().is_none());
    }
}
