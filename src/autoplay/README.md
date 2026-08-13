# `src/autoplay/` — bot decisions → actions in the real client

Translates the bot's chosen mjai action into something the game client
actually does, dispatched over CDP. Only active when
`autoplay.enabled = true` **and** the chromium capture backend is running
(the MITM path has no page handle at all).

Two very different routes in, decided by `platform.kind`:

| Platform | Route | Why |
|---|---|---|
| Mahjong Soul | synthesised mouse input at reconstructed coordinates | The client renders to a canvas and exposes nothing to script. |
| Tenhou | the client's own DOM buttons and discard handler | The client is HTML and script, so its input path can be driven directly. |

Almost everything intricate in this module — coordinate tables, candidate
row index arithmetic, click verification, retries, page reloads — exists
because of the first route. The Tenhou path needs none of it: it presses
what the user would press, and an action that did not land says so. What
both share is the delay model, because a decision executed the instant the
bot answers is the most obvious tell there is.

## Module map

| Module | Role |
|---|---|
| `manager.rs` | Long-lived task: subscribes to `BotResponseBus` + `MjaiBus`, owns per-game state (`last_kawa_tile`, riichi flags, reach two-step), executes click plans. |
| `platform.rs` | `PlatformAutoplay` trait + `ActionContext`/`PlanResult`/`Step` (`Click` / `Sleep` / `AwaitReady` / `DomClick` / `Discard`). The manager only knows this trait. |
| `majsoul/` | The Majsoul implementation: 16:9 coordinate tables (`coords.rs`) + plan dispatch for every mjai action type. |
| `tenhou/` | The Tenhou implementation: press the client's own action buttons (`Step::DomClick`) or call its discard handler with a tile index (`Step::Discard`). `tenhou/inject.rs` is what makes the latter reachable. |
| `tenhou_state.rs` | `TenhouState` — the hand at Tenhou tile-index resolution plus the current decision window (the server's `t` bitmask). Written by the Tenhou bridge, read here. Without it an mjai tile *string* cannot be resolved to the physical copy Tenhou wants. |
| `budget.rs` | `TimeBudget` — the server's per-decision-window time grant (`OptionalOperationList.time_fixed/time_add`, ms). Written by the Majsoul bridge, read here. Never locally accounted; always the server's own value. |
| `delay/` | The pre-click "thinking time" model. See below. |
| `context.rs` | `AutoplayContext` — shared slots between the capture backend, the bridge and the manager (`page`, `canvas_rect`, `time_budget`, `input_watch`, `tenhou_state`). |
| `cdp_input.rs` | chromiumoxide wrappers: hover → press → hold → release click sequence (optionally with a mid-press cursor jiggle for retries), canvas rect query, and the Tenhou route's DOM helpers — action-button selectors, the readiness probe, and the call into the injected discard handler. |
| `verify.rs` | Majsoul only. `InputWatch` — counts the client's own uplink input commands (`inputOperation` / `inputChiPengGang`, bumped by the Majsoul bridge). The manager takes a ticket before pressing; if the count never moves, the click was swallowed and the plan is pressed again (bounded by `click_retries`, gated on the decision window still being live). Repeated dead decisions trigger a page reload (`reload_after_failures`), which reconnects into the hand via the bridge's `GameRestore` path. |

## What arrives on the `BotResponseBus`

Every response the manager plans against is one the riichi engine asked
for. `bot::manager` only flushes a batch to the bot where the engine says
our seat can act, so an `MjaiEvent::None` reaching autoplay is a *decline*
— press pass — and never "the bot had nothing to say about someone else's
turn". That distinction is not visible in the reply itself, which is why it
is settled upstream; see `src/bot/README.md` → "Deciding what to ask".

It has to be settled somewhere. One Tenhou frame can carry three seats'
actions, and when the bot was asked about all of them, the fillers answered
first: a real `hora` arrived to find a pass already pressed into its window.

## The Tenhou route (`tenhou/`)

Tenhou's client owns its board state, and its receive path deliberately
ignores the server's echo of *our own* discard
(`1==U.a && "D"==c.tag || Nb.cb(c)`) because its own handler already
applied it locally. Writing frames onto the socket behind its back
therefore froze the board from the first discard on — observed in a live
game. So actions go through the client's own handlers.

| Action | How | Coordinates? |
|---|---|---|
| chi / pon / kan / riichi / ron / tsumo / kyuushu / kita / pass | click `button.s7[name="c22-<slot>"]` | no |
| discard | call the client's `c21` handler with the tile index | no |

**Buttons.** The client routes clicks on `class="s7"` elements through a
body-level listener into its own handler, so a dispatched click is
indistinguishable from the user pressing one. Slot numbering comes from
the client's menu builder: 0 tsumo, 1 ron, 2 riichi, 3 kyuushu, 4 pass,
5–9 kita, 10–12 kan, 13 daiminkan, 14/15 pon, 16–21 chi.

A slot is not one action, though — several hold *variants*, and which
variants the client draws depends on the hand. Getting this from the
client's own menu builder rather than from what a few live windows happened
to show is what makes it right, because the common case (one variant) looks
the same under every wrong theory:

- **Pon.** Slot 15 is the pon, and it spends a red five whenever the hand
  holds one. Slot 14 is drawn *only* when there is a choice to make — one
  red copy and two plain ones — and it is the pair that leaves the red out.
  So a pon spending a red asks for 15 alone; one that does not asks for 14
  and settles for 15, which is the same pon whenever 14 was not drawn.
- **Chi.** Each of the three shapes (called tile lowest / middle / highest)
  is a pair: the odd slot is the shape made of plain copies, the even one
  below it spends a red five. Unlike pon, either can be drawn without the
  other, so each resolves to exactly one slot and never falls back.
- **Kan.** The three slots hold whatever kans are on offer, packed densely
  in the order the client builds them: every pon we hold the fourth copy of
  (in the order those melds were called), then every concealed set of four
  by ascending tile class. The button says nothing about its tile, so
  telling two kans apart means rebuilding that list — `tenhou::kan_slots`.
- **Kita.** Sanma's North is assigned first of its family, so the family is
  tried in the client's own fill order.

Order matters twice over, because the client appends its buttons **highest
slot first** (`Object.keys(k).sort((a,n)=>n-a)`). A comma-joined selector
would therefore resolve through `querySelector`'s document order and return
the *highest* match — so a plan carries a list of selectors and
`cdp_input::click_dom` walks it in the caller's order. (This is visible in
the logs: a missed press reports the offered slots as the client holds
them, e.g. `[15, 4]`.)

**The discard** has no DOM element, but it needs no position either. The
client's `c21` handler takes a tile *index* and does the rest — sends the
frame and updates its own board. It lives inside the client's IIFE, so
[`tenhou::inject`] rewrites the script in flight to publish the registry
holding it; see that module for how the name is recovered rather than
hard-coded. This is why nothing here computes canvas geometry, and why a
call moving the hand cannot affect it.

**Timing** is the part that had to be learned. Frame arrival is not the
start of the turn: Tenhou's server sends as fast as the seats answer —
against instant opponents, three seats' actions in one burst — and the
client then animates for seconds before drawing buttons or starting its
clock. Every plan therefore opens with `Step::AwaitReady`, which waits for
the client's clock to appear, and the think time is measured from there.
Button presses additionally poll for their element, because the window in
which it exists is bounded by animation on one side and resolution on the
other.

Both action steps are dropped when the decision window they were planned
against is no longer the live one (its `opened_at` is its identity): a
button press because the client would no longer be offering it anyway, and
a discard because the client's handler is *not* guarded — it applies the
tile to the local board whether or not it is our turn, so a stale call
desyncs the board rather than merely wasting a frame. The one exemption is
the riichi tile: the declaration press itself replaces the window (the
server acks with `REACH step=1` and the bridge re-opens it), so for that
plan the moved window is expected, and the tile is still owed.

**Riichi** is one plan, not two: the declaration button and then the tile
the bot named on its `Reach`. The client takes them as separate inputs and
sits on its clock until it has both, and nothing prompts a second bot
decision — the `reach` the server echoes back is our own action coming
home, not a new question.

`Reach.pai` is a V3 extension; mjai itself has no such field, and the
original design took the other route — `inject_reach_for_followup` posts a
synthetic `Reach` back onto the `MjaiBus` so the bot answers with the
declaring dahai. **That route does not work and never has**: `BotManager`
does not treat a `reach` as a decision point (it never has, including in
the commit that added the injection), so the synthetic event is buffered
and the bot is never asked. Nothing has noticed because the built-in bot
always fills `pai` — it resolves the riichi discard before replying and
declines the reach outright if it cannot.

So today a bot that declares riichi without naming the tile cannot riichi
under autoplay on **either** platform. Tenhou skips the declaration whole
and says so in the log — pressing the button without a tile to follow is
worse than not pressing it, because at clock expiry the *client* completes
the riichi itself by throwing the drawn tile, committing the hand to a
wait the bot never chose. The same refusal applies when the tracked hand
cannot produce the tile the bot named. Tracked as issue #257, along with
why the obvious repair — making our own `reach` a decision point — needs
care on Mahjong Soul, where the bridge emits `reach` and the committing
`dahai` together.

### Still to verify against a live game

- **Riichi.** Once riichi is *accepted* the planner stops issuing
  discards, assuming the client auto-discards for itself. If it does not,
  autoplay stalls for the rest of the hand rather than misplaying it.
- **The two-variant windows** — a pon with three copies including a red, a
  chi where both the plain and red copy are held, two kans at once. The
  mapping is read off the client's builder rather than guessed, but no live
  hand has offered one yet.

## The delay model (`delay/`)

`delay::decide` computes a target **total** thinking time for the
current decision window — not a sleep length. The caller subtracts what
the window has already consumed before emitting the `Step::Sleep`:
`majsoul::push_pre_delay` deducts network, proxy and bot inference time
from the server's own budget, plus the upcoming click sequence's duration.
`tenhou::push_pre_delay` has neither to deduct — Tenhou publishes no
per-window budget and nothing is being clicked — so the target *is* the
sleep.

Layers, in order:

1. **Policy** — selected by `autoplay.delay.mode`. `lua` (default):
   `delay.lua` next to the config file, generated from
   `assets/delay_default.lua` (embedded as `script::DEFAULT_SCRIPT`) on
   first use and hot-reloaded. An existing `delay.lua` that is an
   unmodified copy of an *older* bundled default is replaced by the
   current one on start; user-edited scripts are never touched. This
   works by verbatim comparison against every previously shipped
   default, embedded from `assets/delay_default_superseded/` — **when
   changing `assets/delay_default.lua`, copy the version being replaced
   into that directory and add it to `script::SUPERSEDED_DEFAULT_SCRIPTS`**,
   or existing installs will keep the old behaviour. The built-in model
   (per-decision-kind
   log-normal + decision-type bonuses, `delay/mod.rs`) is the fallback
   when the script fails. `legacy`: the historical uniform draw — the
   distribution is forced to Uniform and the script is not consulted.
   The default log-normal parameters are **calibrated against real
   ranked-game records** (think times measured on the server-side
   action clock of decoded game records); change them only with new
   measurements. Both default policies deliberately **ignore the bot's
   probability distribution**: measured bot policies can be nearly flat
   (top ~0.11 on most decisions), which turned every confidence-based
   rule into a permanent bias instead of a signal. Normalized probs
   (`delay/probs.rs`: native-bot `show.items[].prob` raw, Mortal-style
   `q_values` softmaxed — never feed raw Q values to a probability
   threshold) are still exposed to user Lua scripts as
   `ctx.top_prob`/`ctx.second_prob`/`ctx.margin` for custom rules tuned
   to a specific bot.
2. **Budget caps** (`budget_cap`) — `soft = time_fixed - safety_margin -
   click_overhead`; `hard = soft + bounded bank spend`, bank only when
   the policy returned `allow_bank`. Unconditional: overrunning the
   window means the client auto-discards.
3. **Functional floors** (`functional_floor`) — `min_delay_ms` (Mahjong
   Soul must render buttons/tiles before a click can land; too-early
   clicks are silently lost) and the dealing-animation wait. Applied
   after the caps; a script cannot go below them.

The policy's output is the target **server-observed total** for the
window. `majsoul::push_pre_delay` converts it to a sleep by deducting
both the time already elapsed (network + bot inference, from the budget
snapshot) and the upcoming click sequence's own duration (hover + hold +
candidate clicks), then re-applies the functional floor so the first
click still lands after the UI is ready.

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

- Restricted stdlib: math/string/table, plus the base library scrubbed
  of `load`/`loadstring`/`dofile`/`loadfile` (bytecode and filesystem
  escapes), `pcall`/`xpcall` (they could swallow the runaway-guard
  abort) and `string.dump`. No io/os/debug.
- Hard allocation ceiling (`set_memory_limit`) — an allocation bomb is
  a catchable Lua error, not a process abort.
- Instruction budget + wall-clock deadline via VM hook, active for the
  chunk's top level at load time and for every call; if the hook cannot
  be installed the script is not run at all.
- Every failure falls back to the built-in model and logs **once** per
  distinct error. A missing script file is a normal state, not an error.
- Hot reload by mtime+size; a broken file is not recompiled until it
  changes.
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
