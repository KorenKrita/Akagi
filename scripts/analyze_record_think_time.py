#!/usr/bin/env python3
"""Extract per-decision think times and time-bank behaviour from Majsoul
game records captured in Akagi inspector logs.

Game records (`.lq.Lobby.fetchGameRecord` responses) are a far better
calibration source than live-frame deltas:

  * `GameDetailRecords` v210715 timestamps every action with `passed`,
    a cumulative server-side game clock in milliseconds, and records the
    player's raw input (`GameUserInput`) separately from its resulting
    action — so think time is measured on the server's own clock.
  * Every decision window's `OptionalOperationList` is present for
    every seat (unlike live play, where the server only sends your own),
    including `time_fixed` / `time_add` — the remaining time bank per
    seat per window is directly observable.
  * Records of ranked rooms are real humans at a known rank.

The report answers, from real ranked games:
  Q1  the per-window base time (and the dealer-opening allowance)
  Q2  whether `time_add` drains as it is used (and at what granularity)
  Q3  whether the bank resets per kyoku or persists per hanchan
  Q5  the think-time distribution per decision type

Usage:
    python3 scripts/analyze_record_think_time.py --logs <dir>

`<dir>` is searched recursively for `inspector.jsonl`; every
`fetchGameRecord` response found is decoded (deduped by game uuid).
No third-party dependencies; the protobuf wire decoding is inlined.
Output contains seats and rank levels but no nicknames or account ids.
"""

from __future__ import annotations

import argparse
import base64
import collections
import json
import math
import pathlib
import statistics
import sys

# --------------------------------------------------------------------------
# Minimal protobuf wire decoding
# --------------------------------------------------------------------------


def _read_varint(b: bytes, i: int) -> tuple[int, int]:
    r = s = 0
    while True:
        x = b[i]
        i += 1
        r |= (x & 0x7F) << s
        if not x & 0x80:
            return r, i
        s += 7


def decode_msg(b: bytes) -> dict[int, list]:
    """Decode one message into {field_no: [raw values]}. Varints stay ints,
    length-delimited fields stay bytes; nested messages are decoded lazily
    by the caller."""
    out: dict[int, list] = {}
    i = 0
    while i < len(b):
        tag, i = _read_varint(b, i)
        fno, wt = tag >> 3, tag & 7
        if wt == 0:
            v, i = _read_varint(b, i)
        elif wt == 2:
            ln, i = _read_varint(b, i)
            v = b[i : i + ln]
            i += ln
        elif wt == 5:
            v = b[i : i + 4]
            i += 4
        elif wt == 1:
            v = b[i : i + 8]
            i += 8
        else:
            raise ValueError(f"unsupported wire type {wt}")
        out.setdefault(fno, []).append(v)
    return out


def one(m: dict[int, list], fno: int, default=None):
    return m[fno][0] if fno in m else default


def op_list(raw: bytes) -> dict:
    """OptionalOperationList { seat=1, operation_list=2, time_add=4,
    time_fixed=5 } -> {seat, add, fixed, n_ops}."""
    m = decode_msg(raw)
    return {
        "seat": one(m, 1, 0),
        "add": one(m, 4, 0),
        "fixed": one(m, 5, 0),
        "n_ops": len(m.get(2, [])),
    }


# --------------------------------------------------------------------------
# Record extraction
# --------------------------------------------------------------------------


def find_logs(root: pathlib.Path) -> list[pathlib.Path]:
    if root.is_file():
        return [root]
    return [p for p in sorted(root.rglob("inspector.jsonl")) if p.stat().st_size > 0]


def iter_game_records(paths: list[pathlib.Path]):
    """Yield (uuid, head_dict, GameDetailRecords_fields) per distinct game."""
    seen: set[str] = set()
    for path in paths:
        with path.open(errors="replace") as fh:
            for line in fh:
                if ".lq.Lobby.fetchGameRecord" not in line or '"down"' not in line:
                    continue
                try:
                    entry = json.loads(line)
                except json.JSONDecodeError:
                    continue
                payload = (
                    (entry.get("parsed") or {}).get("args", {}).get("payload", {})
                )
                data = payload.get("data")
                head = payload.get("head") or {}
                uuid = head.get("uuid", "?")
                if not data or uuid in seen:
                    continue
                seen.add(uuid)
                try:
                    wrapper = decode_msg(base64.b64decode(data))
                    name = one(wrapper, 1, b"").decode()
                    if name != ".lq.GameDetailRecords":
                        continue
                    gdr = decode_msg(one(wrapper, 2, b""))
                except Exception as e:  # malformed blob — skip, keep going
                    print(f"  ! {uuid}: undecodable record ({e})", file=sys.stderr)
                    continue
                yield uuid, head, gdr


# GameAction { passed=1, type=2, result=3(Wrapper), user_input=4, user_event=5 }
# GameUserInput { seat=1, type=2, emo=3, ... }
INPUT_SELF_TURN = 2  # discard / self-turn operation
INPUT_CLAIM = 3  # response to a claim/chankan window (call or pass)


def parse_actions(gdr: dict[int, list]):
    """Yield normalized dicts per action, in order."""
    for raw in gdr.get(3, []):
        am = decode_msg(raw)
        entry = {"passed": one(am, 1, 0), "atype": one(am, 2, 0)}
        if 3 in am:
            rw = decode_msg(one(am, 3))
            entry["name"] = one(rw, 1, b"").decode().rsplit(".", 1)[-1]
            entry["msg"] = decode_msg(one(rw, 2, b""))
        if 4 in am:
            um = decode_msg(one(am, 4))
            entry["input_seat"] = one(um, 1, 0)
            entry["input_type"] = one(um, 2, 0)
        yield entry


# --------------------------------------------------------------------------
# Stats helpers
# --------------------------------------------------------------------------


def describe(values_ms: list[int]) -> str:
    v = sorted(x for x in values_ms if 0 <= x < 120_000)
    if len(v) < 3:
        return f"n={len(v)} (too few)"

    def q(f: float) -> int:
        return v[min(len(v) - 1, int(len(v) * f))]

    ln = [math.log(max(x, 1) / 1000) for x in v]
    mu = statistics.mean(ln)
    sigma = statistics.pstdev(ln) if len(ln) > 1 else 0.0
    return (
        f"n={len(v):5} min={v[0]:6} p10={q(.10):6} p25={q(.25):6} "
        f"med={int(statistics.median(v)):6} p75={q(.75):6} p90={q(.90):6} "
        f"p99={q(.99):6} max={v[-1]:6} | lognormal mu={mu:+.3f} sigma={sigma:.3f}"
    )


def junme_band(n: int) -> str:
    if n < 6:
        return "j01-06"
    if n < 12:
        return "j07-12"
    return "j13+"


def tile_class(tile: str) -> str:
    """Majsoul tile string -> honor / terminal / middle. '0m' is red 5."""
    if not tile or len(tile) < 2:
        return "?"
    num, suit = tile[0], tile[1]
    if suit == "z":
        return "honor"
    if num in "19":
        return "terminal"
    return "middle"


def bank_bucket(add_ms: int) -> str:
    if add_ms >= 20_000:
        return "bank-full"
    if add_ms >= 5_000:
        return "bank-mid"
    return "bank-low"


def mixture(values_ms: list[int], split_ms: int = 1200) -> str:
    """Two-component summary: how much of the mass is a fast 'routine'
    response vs a real think, and each half's median."""
    v = sorted(x for x in values_ms if 0 <= x < 120_000)
    if len(v) < 10:
        return f"n={len(v)} (too few)"
    fast = [x for x in v if x < split_ms]
    slow = [x for x in v if x >= split_ms]
    frac = len(fast) / len(v)
    med_f = fast[len(fast) // 2] if fast else 0
    med_s = slow[len(slow) // 2] if slow else 0
    return (
        f"n={len(v):5} fast(<{split_ms}ms)={frac:5.1%} med_fast={med_f:5} "
        f"med_slow={med_s:6}"
    )


# --------------------------------------------------------------------------
# Per-game walk
# --------------------------------------------------------------------------


class GameWalk:
    """Replays one game's actions, pairing every USER_INPUT with the
    decision window it answers and tracking each seat's time bank."""

    def __init__(self, buckets, bank_events, fixed_by_kind):
        self.buckets = buckets
        self.bank_events = bank_events  # (window_kind, spent_ms, add_before, add_after)
        self.fixed_by_kind = fixed_by_kind  # Counter[(window_kind, fixed_ms)]
        # per-seat open window: (passed_open, kind)
        self.window: dict[int, tuple[int, str]] = {}
        self.riichi: set[int] = set()
        self.discards = collections.Counter()
        self.kyoku_index = -1
        self.dealer = None
        # bank tracking: seat -> (last add value, window kind it was granted for)
        self.last_add: dict[int, int] = {}
        self.pending_spend: dict[int, tuple[str, int, int]] = {}
        # per-seat time_add trajectory: list of (kyoku_index, add)
        self.add_traj: dict[int, list] = collections.defaultdict(list)

    def note_ops(self, kind: str, ops: list[dict], passed: int):
        for op in ops:
            seat = op["seat"]
            self.fixed_by_kind[(kind, op["fixed"])] += 1
            self.add_traj[seat].append((self.kyoku_index, op["add"]))
            # Close the loop on a previous spend: bank observed before/after.
            if seat in self.pending_spend:
                pkind, spent, add_before = self.pending_spend.pop(seat)
                self.bank_events.append((pkind, spent, add_before, op["add"]))
            self.last_add[seat] = op["add"]
            self.window[seat] = (passed, kind)

    def input_arrived(self, seat: int, itype: int, passed: int):
        if itype not in (INPUT_SELF_TURN, INPUT_CLAIM):
            return
        if seat not in self.window:
            return
        opened, kind = self.window.pop(seat)
        spent = passed - opened
        if seat in self.riichi:
            state = "in-riichi"
        elif self.riichi:
            # Someone else has declared: every decision is now a defence
            # read, not just hand-building.
            state = "vs-riichi"
        else:
            state = "open"
        band = junme_band(self.discards[seat]) if kind in ("draw", "post-call") else "-"
        bank = bank_bucket(self.last_add[seat]) if seat in self.last_add else "?"
        # tsumogiri/riichi/tile flags are refined by the following record;
        # store a mutable row so `classify_discard` can amend it.
        # Row: [kind, giri, state, band, spent, tile_class, bank]
        row = [kind, "-", state, band, spent, "-", bank]
        self.buckets.append(row)
        self.last_row_by_seat = getattr(self, "last_row_by_seat", {})
        self.last_row_by_seat[seat] = row
        if seat in self.last_add:
            self.pending_spend[seat] = (kind, spent, self.last_add[seat])

    def classify_discard(
        self, seat: int, moqie: bool, declares_riichi: bool, tile: str
    ):
        row = getattr(self, "last_row_by_seat", {}).get(seat)
        if row is not None and row[0] in ("draw", "post-call", "dealer-opening"):
            row[1] = "tsumogiri" if moqie else "tedashi"
            if declares_riichi:
                row[2] = "declares-riichi"
            row[5] = tile_class(tile)

    def walk(self, actions):
        for a in actions:
            passed = a["passed"]
            name = a.get("name")
            msg = a.get("msg")

            if "input_seat" in a:
                self.input_arrived(a["input_seat"], a["input_type"], passed)
                continue
            if name is None:
                continue

            if name == "RecordNewRound":
                self.kyoku_index += 1
                self.window.clear()
                self.riichi.clear()
                self.discards.clear()
                self.pending_spend.clear()
                self.dealer = one(msg, 2, 0)
                ops = [op_list(b) for b in msg.get(19, [])]
                if not ops and 12 in msg:
                    ops = [op_list(one(msg, 12))]
                self.note_ops("dealer-opening", ops, passed)
                if self.dealer not in self.window:
                    self.window[self.dealer] = (passed, "dealer-opening")

            elif name == "RecordDealTile":
                seat = one(msg, 1, 0)
                ops = [op_list(one(msg, 8))] if 8 in msg else []
                self.note_ops("draw", ops, passed)
                if seat not in self.window:
                    self.window[seat] = (passed, "draw")

            elif name == "RecordDiscardTile":
                seat = one(msg, 1, 0)
                moqie = bool(one(msg, 5, 0))
                liqi = bool(one(msg, 3, 0)) or bool(one(msg, 9, 0))
                tile = one(msg, 2, b"").decode(errors="replace")
                self.classify_discard(seat, moqie, liqi, tile)
                self.discards[seat] += 1
                if liqi:
                    self.riichi.add(seat)
                ops = [op_list(b) for b in msg.get(10, [])]
                self.note_ops("claim", ops, passed)

            elif name == "RecordChiPengGang":
                seat = one(msg, 1, 0)
                # Losing claimants' windows are void now.
                for s in [s for s, w in self.window.items() if w[1] == "claim"]:
                    del self.window[s]
                ops = [op_list(one(msg, 8))] if 8 in msg else []
                self.note_ops("post-call", ops, passed)
                if seat not in self.window:
                    self.window[seat] = (passed, "post-call")

            elif name == "RecordAnGangAddGang":
                ops = [op_list(b) for b in msg.get(7, [])]
                self.note_ops("chankan", ops, passed)

            elif name == "RecordBaBei":
                ops = [op_list(b) for b in msg.get(7, [])]
                self.note_ops("claim", ops, passed)

            elif name in ("RecordHule", "RecordNoTile", "RecordLiuJu"):
                self.window.clear()
                self.pending_spend.clear()


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--logs", required=True, type=pathlib.Path,
                    help="directory searched recursively for inspector.jsonl")
    args = ap.parse_args()

    paths = find_logs(args.logs)
    if not paths:
        print(f"no inspector.jsonl under {args.logs}", file=sys.stderr)
        return 1

    buckets: list[list] = []
    bank_events: list[tuple] = []
    fixed_by_kind: collections.Counter = collections.Counter()
    trajs = []
    games = 0

    for uuid, head, gdr in iter_game_records(paths):
        version = one(gdr, 2, 0)
        if version < 210715 or not gdr.get(3):
            print(f"  ! {uuid}: old record format (version {version}) — skipped")
            continue
        games += 1
        levels = [
            a.get("level", {}).get("id") for a in head.get("accounts", [])
        ]
        cfg = head.get("config", {})
        print(
            f"  game {games}: {uuid}  category={cfg.get('category')} "
            f"mode_id={cfg.get('meta', {}).get('mode_id')} levels={levels}"
        )
        walk = GameWalk(buckets, bank_events, fixed_by_kind)
        walk.walk(parse_actions(gdr))
        trajs.append((uuid, walk.add_traj))

    if not games:
        print("no decodable game records found")
        return 1

    print("\n" + "=" * 78)
    print("Q1 — PER-WINDOW BASE TIME (time_fixed by window kind, ms)")
    print("=" * 78)
    for (kind, fixed), count in sorted(fixed_by_kind.items()):
        print(f"  {kind:16} time_fixed={fixed:<8} n={count}")

    print("\n" + "=" * 78)
    print("Q2 — BANK DRAIN (spent vs time_add before/after, per window kind)")
    print("=" * 78)
    # Group by (kind, drop) and show the spent range that produced each
    # drop — the boundary spent value reveals the true base allowance.
    grouped = collections.defaultdict(list)
    for kind, spent, before, after in bank_events:
        grouped[(kind, before - after)].append(spent)
    for (kind, drop), spents in sorted(grouped.items()):
        spents.sort()
        print(
            f"  {kind:16} drop={drop:<7} n={len(spents):5} "
            f"spent_min={spents[0]:6} spent_med={spents[len(spents)//2]:6} "
            f"spent_max={spents[-1]:6}"
        )

    print("\n" + "=" * 78)
    print("Q3 — BANK TRAJECTORY ACROSS KYOKU (does time_add reset?)")
    print("=" * 78)
    for uuid, traj in trajs[:3]:
        print(f"  {uuid}")
        for seat in sorted(traj):
            seq = traj[seat]
            # Compress: one entry per kyoku (first and last observed).
            per_k = collections.OrderedDict()
            for k, add in seq:
                per_k.setdefault(k, [add, add])[1] = add
            desc = " ".join(f"k{k}:{v[0]//1000}->{v[1]//1000}s" for k, v in per_k.items())
            print(f"    seat {seat}: {desc}")

    print("\n" + "=" * 78)
    print("Q5 — THINK TIME (server clock, real ranked players)")
    print("=" * 78)
    agg = collections.defaultdict(list)
    for kind, giri, state, band, spent, _tc, _bank in buckets:
        agg[(kind, giri, state, band)].append(spent)
    for key in sorted(agg):
        kind, giri, state, band = key
        label = f"{kind:14} {giri:9} {state:15} {band:6}"
        print(f"  {label} {describe(agg[key])}")

    print("\n" + "=" * 78)
    print("Q5a — TEDASHI BY TILE CLASS (draw windows, no riichi on table)")
    print("=" * 78)
    agg = collections.defaultdict(list)
    for kind, giri, state, band, spent, tc, _bank in buckets:
        if kind == "draw" and giri == "tedashi" and state == "open":
            agg[(tc, band)].append(spent)
    for key in sorted(agg):
        tc, band = key
        print(f"  {tc:9} {band:6} {describe(agg[key])}")
    print()
    print("  Routine-vs-decision structure (fast fraction under 1.2s):")
    agg2 = collections.defaultdict(list)
    for kind, giri, state, band, spent, tc, _bank in buckets:
        if kind == "draw" and state == "open" and giri in ("tedashi", "tsumogiri"):
            agg2[(giri, tc)].append(spent)
    for key in sorted(agg2):
        giri, tc = key
        print(f"  {giri:9} {tc:9} {mixture(agg2[key])}")

    print("\n" + "=" * 78)
    print("Q5b — DEFENCE: THINK TIME WITH AN OPPONENT RIICHI ON THE TABLE")
    print("=" * 78)
    agg = collections.defaultdict(list)
    for kind, giri, state, band, spent, _tc, _bank in buckets:
        if kind == "draw" and giri in ("tedashi", "tsumogiri") and state in ("open", "vs-riichi"):
            agg[(giri, state)].append(spent)
    for key in sorted(agg):
        giri, state = key
        print(f"  {giri:9} {state:9} {describe(agg[key])}")

    print("\n" + "=" * 78)
    print("Q5c — URGENCY: THINK TIME BY REMAINING TIME BANK")
    print("=" * 78)
    agg = collections.defaultdict(list)
    for kind, giri, state, band, spent, _tc, bank in buckets:
        if kind == "draw" and giri in ("tedashi", "tsumogiri"):
            agg[(giri, bank)].append(spent)
    for key in sorted(agg):
        giri, bank = key
        print(f"  {giri:9} {bank:9} {describe(agg[key])}")

    print()
    print("  'claim' rows include declined windows (the pass itself is the")
    print("  recorded input) — a dimension live-frame captures cannot see.")
    print("  Times are on the server's game clock; the only residual bias is")
    print("  each player's own client->server latency.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
