#!/usr/bin/env python3
"""Analyse Majsoul decision-window budgets and per-seat think times from
Akagi inspector logs.

Reads `inspector.jsonl` files (the inspector records every WebSocket frame
unconditionally, with the protobuf already decoded to JSON) and reports:

  Part 1 — time budget
      Which actions carry `OptionalOperationList`, the `time_fixed` /
      `time_add` values they carry, whether the opening-hand allowance is
      an absolute or proportional bonus, and how `time_add` moves over the
      course of a game.

  Part 2 — think times
      Decision latency per seat, split into the dimensions the delay model
      needs: draw->discard vs call->discard (post-call) vs discard->call
      (naki window), tsumogiri/tedashi, riichi state, and junme band
      (early/mid/late kyoku). Opponent seats are the useful sample here:
      three players per hanchan instead of one, drawn from the population
      rather than from whoever is running this tool.

IMPORTANT — sampling validity
    A room full of Majsoul's own AI produces timings quantised onto a
    ~500ms grid, and a session recorded while doing something else at the
    same time produces a meaningless tail. Neither is a usable model of
    human play. This tool detects AI rooms from `game_config` and refuses
    to report Part 2 for them unless `--force` is given; it cannot detect
    inattentive play, so only point it at sessions that were real games.

Usage:
    python3 scripts/analyze_autoplay_timing.py --logs <dir> [--force]

`<dir>` is searched recursively for `inspector.jsonl`.
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import pathlib
import statistics
import sys

# --------------------------------------------------------------------------
# Frame walking
# --------------------------------------------------------------------------

ACTIONS_WITH_OPERATION = (
    "ActionDealTile",
    "ActionDiscardTile",
    "ActionNewRound",
    "ActionChiPengGang",
)


def iter_frames(path: pathlib.Path):
    """Yield (ts_ms, direction, method, action_name, data, payload) per frame.

    `mjai_event` entries are surfaced as a synthetic frame with the action
    name `#mjai:<type>` and the event itself as `data`, so callers can pick
    our seat out of `start_game` without a second pass. Frames the inspector
    could not parse are skipped silently — the log also carries lobby
    traffic, heartbeats and non-protobuf payloads.
    """
    with path.open(errors="replace") as fh:
        for line in fh:
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            kind = entry.get("kind")
            if kind == "mjai_event":
                event = entry.get("event") or {}
                yield (
                    entry.get("ts_ms"),
                    "mjai",
                    "",
                    f"#mjai:{event.get('type')}",
                    event,
                    {},
                )
                continue
            if kind != "ws_frame":
                continue
            parsed = entry.get("parsed") or {}
            payload = (parsed.get("args") or {}).get("payload")
            if not isinstance(payload, dict):
                payload = {}
            data = payload.get("data")
            yield (
                entry.get("ts_ms"),
                entry.get("direction"),
                parsed.get("method") or "",
                payload.get("name"),
                data if isinstance(data, dict) else None,
                payload,
            )


def find_logs(root: pathlib.Path) -> list[pathlib.Path]:
    if root.is_file():
        return [root]
    found = sorted(root.rglob("inspector.jsonl"))
    return [p for p in found if p.stat().st_size > 0]


def room_kind(path: pathlib.Path) -> tuple[bool | None, set]:
    """Return (is_ai_room, room_descriptors) from any `game_config` seen."""
    ai_flags = set()
    descriptors = set()

    def walk(obj):
        if isinstance(obj, dict):
            if "category" in obj and "mode" in obj and "meta" in obj:
                mode = obj.get("mode") or {}
                ai_flags.add(bool(mode.get("ai")))
                descriptors.add(
                    (obj.get("category"), mode.get("mode"), bool(mode.get("ai")))
                )
            for v in obj.values():
                walk(v)
        elif isinstance(obj, list):
            for v in obj:
                walk(v)

    for _ts, _dir, _m, _name, _data, payload in iter_frames(path):
        if payload:
            walk(payload)
    if not ai_flags:
        return None, descriptors
    return (True in ai_flags), descriptors


# --------------------------------------------------------------------------
# Stats helpers
# --------------------------------------------------------------------------


def describe(values_ms: list[int]) -> str:
    v = sorted(x for x in values_ms if 0 < x < 120_000)
    if len(v) < 3:
        return f"n={len(v)} (too few)"

    def q(f: float) -> int:
        return v[min(len(v) - 1, int(len(v) * f))]

    ln = [math.log(x / 1000) for x in v]
    mu = statistics.mean(ln)
    sigma = statistics.pstdev(ln) if len(ln) > 1 else 0.0
    return (
        f"n={len(v):5} min={v[0]:6} p10={q(.10):6} p25={q(.25):6} "
        f"med={int(statistics.median(v)):6} p75={q(.75):6} p90={q(.90):6} "
        f"max={v[-1]:6} | lognormal mu={mu:+.3f} sigma={sigma:.3f}"
    )


# --------------------------------------------------------------------------
# Part 1 — time budget
# --------------------------------------------------------------------------


def analyse_budget(paths: list[pathlib.Path]) -> None:
    print("=" * 78)
    print("PART 1 — TIME BUDGET (Q1-Q4)")
    print("=" * 78)

    by_action = collections.Counter()
    op_seat_is_ours = collections.Counter()
    trajectories: list[list[tuple[str, int, int]]] = []

    for path in paths:
        our_seat = None
        current: list[tuple[str, int, int]] = []
        for ts, direction, _method, name, data, _payload in iter_frames(path):
            if name == "#mjai:start_game" and data is not None:
                our_seat = data.get("id")
                continue
            if direction != "down" or name is None or data is None:
                continue
            op = data.get("operation")
            has_op = isinstance(op, dict) and "time_fixed" in op
            if name == "ActionNewRound" and current:
                trajectories.append(current)
                current = []
            if not has_op:
                if name in ACTIONS_WITH_OPERATION:
                    op_seat_is_ours[(name, "no operation")] += 1
                continue
            by_action[(name, op.get("time_fixed"), op.get("time_add"))] += 1
            if our_seat is None:
                label = "operation, our seat unknown"
            elif op.get("seat") == our_seat:
                label = "operation, OUR seat"
            else:
                label = "operation, OTHER seat"
            op_seat_is_ours[(name, label)] += 1
            current.append((name, op.get("time_fixed", 0), op.get("time_add", 0)))
        if current:
            trajectories.append(current)

    print("\n[Q1] (action, time_fixed, time_add) -> count")
    if not by_action:
        print("  (nothing found — no operation lists in these logs)")
        return
    for key, count in sorted(by_action.items(), key=lambda kv: (-kv[1], str(kv[0]))):
        print(f"  {key[0]:20} time_fixed={key[1]:<8} time_add={key[2]:<8} n={count}")

    # Weight by frame count, not by distinct (name, fixed, add) combos —
    # otherwise one rare combo per value skews the mode.
    fixed_regular = collections.Counter()
    fixed_opening = collections.Counter()
    for (name, fixed, _add), count in by_action.items():
        if name in ("ActionDealTile", "ActionDiscardTile"):
            fixed_regular[fixed] += count
        elif name == "ActionNewRound":
            fixed_opening[fixed] += count
    if fixed_regular and fixed_opening:
        base = fixed_regular.most_common(1)[0][0]
        opening = fixed_opening.most_common(1)[0][0]
        delta = opening - base
        ratio = opening / base if base else float("nan")
        print(f"\n[Q1] opening-hand allowance: base={base} opening={opening}")
        print(f"     absolute hypothesis:     +{delta} ms")
        print(f"     proportional hypothesis: x{ratio:.4f}")
        if base not in (300000,):
            print("     -> base differs from the 300000 seen in AI rooms; "
                  "these two hypotheses are now DISTINGUISHABLE. Compare "
                  "against the other captures to settle it.")
        else:
            print("     -> base is still 300000, so +3000 and x1.01 remain "
                  "indistinguishable. Needs a room with a different base.")

    print("\n[Q4] does the server send us other seats' operation lists?")
    for key, count in sorted(op_seat_is_ours.items(), key=lambda kv: str(kv[0])):
        print(f"  {key[0]:20} {key[1]:32} n={count}")
    print("  (an operation list is only ever addressed to the seat that must "
          "act; if 'other seat' is 0 we can only ever observe our own budget)")

    print("\n[Q2/Q3] time_add trajectory per kyoku (first 6 kyoku shown)")
    nonzero = [t for t in trajectories if any(x[2] for x in t)]
    if not nonzero:
        print("  every time_add is 0 in these logs — this room grants no extra")
        print("  time pool, so bank semantics CANNOT be answered from here.")
    else:
        for i, traj in enumerate(nonzero[:6]):
            seq = " ".join(str(x[2]) for x in traj)
            print(f"  kyoku {i}: {seq}")
        print("  -> monotonically decreasing within a kyoku answers Q2;")
        print("     whether the first value of each kyoku returns to the")
        print("     maximum answers Q3 (per-kyoku vs per-hanchan pool).")


# --------------------------------------------------------------------------
# Part 2 — think times
# --------------------------------------------------------------------------


def analyse_think_times(paths: list[pathlib.Path]) -> None:
    print()
    print("=" * 78)
    print("PART 2 — THINK TIMES (Q5)")
    print("=" * 78)

    buckets: dict[tuple, list[int]] = collections.defaultdict(list)

    def junme_band(n: int) -> str:
        # Per-seat discard count -> early / mid / late kyoku.
        if n < 6:
            return "j01-06"
        if n < 12:
            return "j07-12"
        return "j13+"

    for path in paths:
        our_seat = None
        drew: dict[int, int] = {}
        called: dict[int, int] = {}
        discards: dict[int, int] = collections.Counter()
        riichi: set[int] = set()
        new_round_at: tuple[int, int] | None = None
        last_discard_ts: int | None = None

        for ts, direction, _method, name, data, _payload in iter_frames(path):
            if name == "#mjai:start_game" and data is not None:
                our_seat = data.get("id")
                continue
            if direction != "down" or name is None or data is None:
                continue

            if name == "ActionNewRound":
                drew.clear()
                called.clear()
                discards.clear()
                riichi.clear()
                last_discard_ts = None
                oya = data.get("ju")
                new_round_at = (ts, oya) if oya is not None else None

            elif name == "ActionDealTile":
                seat = data.get("seat")
                if seat is not None:
                    drew[seat] = ts

            elif name == "ActionDiscardTile":
                seat = data.get("seat")
                if seat is None:
                    continue
                who = "self" if seat == our_seat else "opponent"
                tsumogiri = "tsumogiri" if data.get("moqie") else "tedashi"
                state = "in-riichi" if seat in riichi else "open"
                band = junme_band(discards[seat])

                if new_round_at and seat == new_round_at[1] and seat not in drew:
                    buckets[(who, "dealer-opening", "-", "-", "-")].append(
                        ts - new_round_at[0]
                    )
                elif seat in called:
                    # Discard following the seat's own chi/pon/daiminkan.
                    # Separate decision type: fewer options, and the clock
                    # reference is the call, not a draw.
                    buckets[(who, "post-call", tsumogiri, state, band)].append(
                        ts - called.pop(seat)
                    )
                elif seat in drew:
                    buckets[(who, "draw-discard", tsumogiri, state, band)].append(
                        ts - drew.pop(seat)
                    )

                discards[seat] += 1
                last_discard_ts = ts
                # The dealer-opening sample is only valid for the very
                # first discard of the kyoku. Clear unconditionally: if the
                # dealer's first action was e.g. an ankan, a stale marker
                # would later misclassify a mid-kyoku discard as a
                # multi-turn "dealer-opening" interval.
                new_round_at = None
                if data.get("is_liqi") or data.get("is_wliqi"):
                    riichi.add(seat)

            elif name == "ActionChiPengGang":
                seat = data.get("seat")
                if seat is None:
                    continue
                # Reaction time of the naki window: victim's discard hits
                # the wire -> the call is broadcast. Only accepted calls are
                # visible; declines are folded into the next draw interval.
                if last_discard_ts is not None:
                    who = "self" if seat == our_seat else "opponent"
                    buckets[(who, "naki-window", "-", "-", "-")].append(
                        ts - last_discard_ts
                    )
                drew.pop(seat, None)
                called[seat] = ts  # call -> follow-up discard

    if not buckets:
        print("  no draw/discard pairs found")
        return

    for key in sorted(buckets):
        who, kind, giri, state, band = key
        label = f"{who:8} {kind:14} {giri:9} {state:9} {band:6}"
        print(f"  {label} {describe(buckets[key])}")

    print()
    print("  'opponent' rows are the calibration target: three seats per")
    print("  hanchan, sampled from the population rather than from whoever")
    print("  ran the capture. 'self' rows are shown for comparison only.")
    print()
    print("  Note: opponent intervals include the server->player RTT, which")
    print("  is invisible to us. It shifts the distribution by a few hundred")
    print("  ms but does not change its shape.")


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--logs", required=True, type=pathlib.Path,
                    help="directory searched recursively for inspector.jsonl")
    ap.add_argument("--force", action="store_true",
                    help="report think times even for AI rooms and sessions "
                         "with no game_config (not a valid human sample; for "
                         "methodology checks only)")
    args = ap.parse_args()

    paths = find_logs(args.logs)
    if not paths:
        print(f"no non-empty inspector.jsonl under {args.logs}", file=sys.stderr)
        return 1
    print(f"reading {len(paths)} log file(s) under {args.logs}\n")

    ai_paths, human_paths, unknown = [], [], []
    for p in paths:
        is_ai, descriptors = room_kind(p)
        if is_ai is True:
            ai_paths.append((p, descriptors))
        elif is_ai is False:
            human_paths.append(p)
        else:
            unknown.append(p)

    print(f"room classification: {len(human_paths)} non-AI, {len(ai_paths)} AI, "
          f"{len(unknown)} unknown (no game_config seen)")

    analyse_budget(paths)

    usable = list(human_paths)
    if ai_paths and not args.force:
        print()
        print(f"!! {len(ai_paths)} session(s) are AI rooms (mode.ai=true) and are")
        print("!! EXCLUDED from Part 2. Majsoul's AI discards on a ~500ms grid;")
        print("!! including it would poison the distribution. Use --force to")
        print("!! override for a methodology check.")
    if unknown and not args.force:
        print()
        print(f"!! {len(unknown)} session(s) carried no game_config and are EXCLUDED")
        print("!! from Part 2 — an unclassifiable AI room would poison the sample.")
        print("!! Use --force to include them.")
    if args.force:
        usable = list(paths)

    if not usable:
        print()
        print("no sessions left for Part 2. Capture a game in a room with real")
        print("opponents and a real time limit, then re-run.")
        return 0

    analyse_think_times(usable)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
