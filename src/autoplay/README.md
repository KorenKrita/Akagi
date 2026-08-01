# `src/autoplay/` — bot decisions → table clicks

Translates the bot's chosen mjai action into UI clicks dispatched over
CDP. Only active when `autoplay.enabled = true` **and** the chromium
capture backend is running (the MITM path has no page handle to click).

## Module map

| Module | Role |
|---|---|
| `manager.rs` | Long-lived task: subscribes to `BotResponseBus` + `MjaiBus`, owns per-game state (`last_kawa_tile`, riichi flags, reach two-step), executes click plans. |
| `platform.rs` | `PlatformAutoplay` trait + `ActionContext`/`PlanResult`/`Step`. The manager only knows this trait; a Tenhou impl would slot in here. |
| `majsoul/` | The production implementation: 16:9 coordinate tables (`coords.rs`) + plan dispatch for every mjai action type. |
| `budget.rs` | `TimeBudget` — the server's per-decision-window time grant (`OptionalOperationList.time_fixed/time_add`, ms). Written by the Majsoul bridge, read here. Never locally accounted; always the server's own value. |
| `delay/` | The pre-click "thinking time" model. See below. |
| `context.rs` | `AutoplayContext` — shared slots between the capture backend, the bridge and the manager (`page`, `canvas_rect`, `time_budget`). |
| `cdp_input.rs` | chromiumoxide wrappers: hover → press → hold → release click sequence, canvas rect query. |

## The delay model (`delay/`)

`delay::decide` computes a target **total** thinking time for the
current decision window — not a sleep length. The caller
(`majsoul::push_pre_delay`) subtracts the time the window has already
consumed (network, proxy, bot inference) before emitting the
`Step::Sleep`.

Layers, in order:

1. **Policy** — selected by `autoplay.delay.mode`. `lua` (default):
   `delay.lua` next to the config file, generated from
   `assets/delay_default.lua` (embedded as `script::DEFAULT_SCRIPT`) on
   first use and hot-reloaded; the built-in model (per-decision-kind
   log-normal + decision-type bonuses, `delay/mod.rs`) is the fallback
   when the script fails. `legacy`: the historical uniform draw — the
   distribution is forced to Uniform and the script is not consulted.
   The default log-normal parameters are **calibrated against real
   ranked-game records** (think times measured on the server-side
   action clock of decoded game records); change them only with new
   measurements. Bot confidence is normalized
   first (`delay/probs.rs`): native-bot `show.items[].prob` is used raw,
   Mortal-style `q_values` are softmaxed. Never feed raw Q values to a
   probability threshold.
2. **Budget caps** (`budget_cap`) — `soft = time_fixed - safety_margin -
   click_overhead`; `hard = soft + bounded bank spend`, bank only when
   the policy returned `allow_bank`. Unconditional: overrunning the
   window means the client auto-discards.
3. **Functional floors** (`functional_floor`) — `min_delay_ms` (Mahjong
   Soul must render buttons/tiles before a click can land; too-early
   clicks are silently lost) and the dealing-animation wait. Applied
   after the caps; a script cannot go below them.

### Adding a new delay input

1. Add the field to `DelayInput` (`delay/mod.rs`) and derive it in
   `majsoul::push_pre_delay` (or the manager, if it needs async state).
2. Consume it in `decide`. Default parameter values must be backed by
   calibration data (record measurements); a knob without data defaults
   to off/no-op.
3. Expose it to Lua in `script::DelayScript::build_ctx` and document it
   in `assets/delay_default.lua` + the README's ctx table.
4. Config knobs go in `config/autoplay.rs::DelayModelConfig` (serde
   defaults!), mirrored in `frontend/src/types.ts` and, if user-facing,
   `Settings.tsx`.

### Lua host invariants (`delay/script.rs`)

- Restricted stdlib (math/string/table). No io/os.
- Instruction budget + wall-clock deadline via VM hook; if the hook
  cannot be installed the script is not run at all.
- Every failure falls back to the built-in model and logs **once** per
  distinct error. A missing script file is a normal state, not an error.
- Hot reload by mtime; a broken file is not recompiled until it changes.
- The script decides *when*, never *what*: it gets no coordinates, no
  action choice, and its output is clamped by caps and floors.

## Time budget flow

```
Majsoul server frame (ActionPrototype with operation for our seat)
  └─ MajsoulBridge::update_time_budget          (bridge/majsoul/mod.rs)
       └─ AutoplayContext.time_budget           (std RwLock slot)
            └─ manager snapshot (elapsed_ms precomputed)
                 └─ ActionContext.budget → delay model caps
```

The bridge clears the slot when an action carries no operation (window
closed) and at game boundaries. `GameRestore` replays park the value and
commit only the final still-open window, backdating `opened_at` by
`passed_waiting_time`. The MITM proxy passes no slot — `budget` is
simply `None` there and the static `no_budget_cap_ms` applies.

## Testing notes

- Delay-model tests are seeded (`StdRng`) — no flaky distributions.
- Use synthetic payloads for bridge tests, never pasted real captures
  with account ids.
- `assets/delay_default.lua` is compiled and exercised by
  `script::tests::bundled_default_script_works`; keep it in sync with
  the ctx table.
