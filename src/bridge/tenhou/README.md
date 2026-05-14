# Tenhou bridge

Translates Tenhou (天鳳) WebSocket frames into the mjai event stream the rest
of AkagiV3 consumes. Used in **observe-only mode**: server → client frames are
parsed; client → server frames and `Bridge::build` are no-ops. The reasoning is
that all game state we need to feed analysis / bots arrives on server frames;
client frames are user input and contribute no new information.

## Wire format at a glance

Tenhou's WS frames are plain JSON: one event per frame, dispatched by `tag`.
The complete tag inventory and field semantics follow the original Akagi
Python Tenhou bridge; this is a faithful Rust port, with comments calling
out any deliberate divergence.

### Tags we handle

| Tag | Trigger | mjai output |
|---|---|---|
| `<Z/>` | heartbeat | (none) |
| `HELO` / `REJOIN` / `GO` / `UN` / `BYE` / `SHUFFLE` | session control | (none) |
| `TAIKYOKU` | start of game | `start_game` (resolves our seat from `oya`) |
| `INIT` | start of kyoku | `start_kyoku` (sanma detected via 0-score slot) |
| `T<n>` / `U<n>` / `V<n>` / `W<n>` | tsumo (rel seats 0..3) | `tsumo` |
| `D<n>` / `E<n>` / `F<n>` / `G<n>` (uppercase) | discard of just-drawn tile | `dahai { tsumogiri: true }` |
| `d<n>` / `e<n>` / `f<n>` / `g<n>` (lowercase) | tedashi | `dahai { tsumogiri: false }` |
| `N` with `m` | call (chi/pon/kan/kakan/nukidora) | `chi` / `pon` / `daiminkan` / `kakan` / `ankan` / `kita` |
| `REACH step=1` | declare riichi | `reach` |
| `REACH step=2` | riichi accepted | `reach_accepted` |
| `DORA` | new dora indicator | `dora` |
| `AGARI` (no `owari`) | win | `hora` + `end_kyoku` |
| `AGARI` (with `owari`) | win at game end | `hora` + `end_kyoku` + `end_game` |
| `RYUUKYOKU` (no `owari`) | exhaustive draw | `ryukyoku` + `end_kyoku` |
| `RYUUKYOKU` (with `owari`) | draw at game end | `ryukyoku` + `end_kyoku` + `end_game` |

### Tile encoding

Tenhou tiles are integer indices `0..=135`. `index / 4` gives tile type
(`0..=33`); `index % 4` is the variant. Red 5s live at exactly `16`, `52`, `88`
(serialize as `5mr`, `5pr`, `5sr`). See `tile.rs`.

### Seat encoding

Tenhou messages always use *relative* seats: rel 0 is the observing player.
`State::rel_to_abs` / `abs_to_rel` translate to mjai's absolute frame. The
bridge resolves our absolute seat from `<TAIKYOKU oya="N"/>`: `seat = (4 - N) % 4`
(`(3 - N) % 3` once sanma is detected at INIT).

### Meld bitfield

`<N m="..."/>` packs the meld kind, target seat, and tile composition into one
integer. Bit decoding lives in `meld.rs` and follows
<http://tenhou.net/img/mentsu136.txt> exactly. Nukidora (北抜き) is the special
case `(m & 0x3F) == 0x20` — handled before the structured decoder.

## Adding a new tag handler

1. Add a `match` arm in `TenhouBridge::dispatch` (`mod.rs`) that routes the new
   tag to a private handler.
2. Implement the handler. It should return `Vec<MjaiEvent>`. If you need new
   per-flow state, add it to `state::State` (and reset in `reset_for_kyoku`
   when appropriate).
3. Add a unit test covering at least one realistic JSON input.

## Why observation only

The Tenhou bridge is intentionally scoped to "look at server messages, ignore
client messages." Autoplay (`Bridge::build`) is intentionally not implemented
— it returns `None`. If you want to add autoplay later you would also need to
revisit `parse(Direction::Up, ..)` to track action acks.

## References

- Bit-level meld spec: <http://tenhou.net/img/mentsu136.txt>
- Tile-image table: <http://tenhou.net/img/tehai.js>
- mjai event types: `src/schema/mjai/mod.rs`
- Sister bridge for protobuf platforms: `src/bridge/majsoul/`
