import { describe, expect, it } from 'vitest'

import { computePt, majsoulDanFromRankId, type PtRule } from './ptCalc'
import type { GameRecord } from '@/types'

const rule: PtRule = { kind: 'majsoul', lobby: 'jade', dan: 'jakugou_1' }

function record(rank: 1 | 4, rankId?: number, score = 25_000): GameRecord {
  return {
    id: String(rankId ?? 'legacy'),
    started_at: '2026-07-26T00:00:00Z',
    ended_at: '2026-07-26T01:00:00Z',
    platform: 'majsoul',
    num_players: 4,
    kyoku_mode: 'east_south',
    names: ['a', 'b', 'c', 'd'],
    our_seat: 0,
    final_scores: [score, 26_000, 25_000, 24_000],
    final_ranks: rank === 1 ? [1, 2, 3, 4] : [4, 1, 2, 3],
    our_rank: rank,
    majsoul_rank_id: rankId,
    our_delta: score - 25_000,
    stats: {} as GameRecord['stats'],
    log_path: 'games/test.mjai.jsonl',
  }
}

function sanmaLast(rankId: number): GameRecord {
  return {
    ...record(4, rankId, 35_000),
    num_players: 3,
    names: ['a', 'b', 'c'],
    final_scores: [35_000, 36_000, 34_000],
    final_ranks: [3, 1, 2],
    our_rank: 3,
    our_delta: 0,
  }
}

describe('Mahjong Soul per-game rank PT', () => {
  it('maps both four-player and three-player level ids', () => {
    expect(majsoulDanFromRankId(10201)).toBe('jakushi_1')
    expect(majsoulDanFromRankId(10503)).toBe('jakusei_3')
    expect(majsoulDanFromRankId(20402)).toBe('jakugou_2')
    expect(majsoulDanFromRankId(10612)).toBe('konten')
    expect(majsoulDanFromRankId(undefined)).toBeNull()
  })

  it('changes the fourth-place penalty at each recorded rank boundary', () => {
    expect(computePt(record(4, 10201), rule)).toBe(-35)
    expect(computePt(record(4, 10503), rule)).toBe(-255)
  })

  it('uses the recorded three-player rank for the last-place penalty', () => {
    expect(computePt(sanmaLast(20201), rule)).toBe(-35)
    expect(computePt(sanmaLast(20503), rule)).toBe(-305)
  })

  it('keeps the selected room because rank does not uniquely determine it', () => {
    expect(computePt(record(1, 10201), rule)).toBe(125)
  })

  it('uses the selected rank only for legacy records', () => {
    expect(computePt(record(4), rule)).toBe(-180)
  })

  it('rounds the platform PT result upward to an integer', () => {
    expect(computePt(record(1, 10401, 25_100), rule)).toBe(126)
  })
})
