# `bridge/riichi_city` — Riichi City (麻雀一番街) protocol bridge

Translates Riichi City's WebSocket traffic into the mjai event stream. Native
client only (no web build), so it is reachable **only through the MITM proxy**;
the Chromium/CDP capture backend does not apply. **Observe-only** — no autoplay.

This is a Rust port of the original Akagi v2 Python bridge
(`mitm/bridge/riichi_city/`). The on-the-wire framing comes from the
riichishitty reverse-engineering notes.

## Files

| File | Responsibility |
|---|---|
| `packet.rs` | `WPacket` framing: 15-byte big-endian header + JSON body decode. |
| `consts.rs` | `card_to_mjai` — Riichi City tile code → mjai tile string. |
| `state.rs` | `GameStatus` — per-flow seat / player / pending state. |
| `mod.rs` | `RiichiCityBridge` — `cmd` dispatch + mjai event building. |

## Wire format (WPacket)

One WebSocket binary message normally carries one WPacket. All multi-byte
header fields are **big-endian**:

```text
[0..4]   packet_size    u32   total bytes including header
[4..6]   header_size    u16   always 15  ─┐ together the magic
[6..8]   version        u16   always 1   ─┘ bytes 00 0f 00 01
[8..12]  message_index  u32   request/response correlation counter
[12..14] cmd            u16   binary command enum (CMDAuth = 1)
[14]     has_body       u8    0 / 1
[15..]   json_payload   UTF-8 JSON (present when packet_size > 15)
```

## Two-level command

- The binary `cmd` matters only for `CMDAuth` (1), whose JSON body carries our
  player `uid` (client → server).
- Every gameplay message carries a **string** `"cmd"` field inside the JSON
  body (`cmd_enter_room`, `cmd_game_start`, `cmd_game_action_brc`, …). Dispatch
  keys on that string.

Both wire directions are parsed (the `uid` packet is uplink; gameplay is
downlink). Client request frames don't match any server `cmd_*` and are ignored.

## cmd → mjai

| Riichi City `cmd` (+ action code) | mjai event(s) |
|---|---|
| `cmd_enter_room` | — (records players, table size) |
| `cmd_game_start` | `start_game` (first kyoku) → `start_kyoku` → opening `tsumo` |
| `cmd_in_card_brc` | `tsumo` (opponent draw, hidden `?`) |
| `cmd_send_current_action` | `tsumo` (our draw, revealed) |
| `cmd_game_action_brc` action 2/3/4 | `chi` |
| … action 5 | `pon` |
| … action 6 | `daiminkan` |
| … action 8 | `ankan` (first slot red for 5s) |
| … action 9 | `kakan` |
| … action 11 | `reach`? + `dahai` (+ deferred `reach_accepted`, kan-dora flush) |
| … action 13 | `kita` (nukidora) |
| … action 7 / 10 / 12 | `end_kyoku` (see "Settlement" below) |
| `cmd_gang_bao_brc` | — (defers a kan-dora marker) |
| `cmd_room_end` | `end_game` |

## Sanma (3-player)

Unlike v2 (which padded everything to length 4 with a ghost seat), this bridge
emits native length-3 `scores`/`tehais` and keeps actor indices in `0..=2`,
matching the Tenhou bridge and the rest of the V3 sanma pipeline.

## Settlement (win / draw) — not yet decoded

Actions 7 (ron), 10 (tsumo-ron) and 12 (exhaustive draw) currently emit only a
bare `end_kyoku`, as v2 did. Decoding the round-result message to emit a full
`hora` (actor / target / deltas / ura-dora) and `ryukyoku` deltas — so game
history win/deal-in/points stats are correct — needs real captures and is
tracked as follow-up work.

## Adding / fixing a `cmd`

1. Find the JSON shape in a capture (or the v2 `bridge.py`).
2. Add a `match` arm in `RiichiCityBridge::dispatch` → a new `on_*` handler.
3. Build mjai events with `field_card` / `card_to_mjai` and the `GameStatus`
   helpers; push to the returned `Vec<MjaiEvent>`.
4. Add a unit test in `mod.rs` using the `feed(..)` helper with **fake** ids.
