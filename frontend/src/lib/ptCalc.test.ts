import { describe, expect, it } from 'vitest'

import { computePt, type PtRule } from './ptCalc'
import type { GameRecord } from '@/types'

const rule: PtRule = { kind: 'majsoul', lobby: 'jade', dan: 'jakugou_1' }

function record(rank: 1 | 4, score = 25_000): GameRecord {
  return {
    id: 'g1',
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
    our_delta: score - 25_000,
    stats: {} as GameRecord['stats'],
    log_path: 'games/test.mjai.jsonl',
  }
}

describe('Mahjong Soul PT', () => {
  it('applies the selected rank penalty to fourth place', () => {
    expect(computePt(record(4), rule)).toBe(-180)
  })

  it('rounds the platform PT result upward to an integer', () => {
    expect(computePt(record(1, 25_100), rule)).toBe(126)
  })
})
