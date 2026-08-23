// Display helpers for `GameRecord.match_info` (room / rank-lobby labels and
// paifu ids). The backend persists raw platform ids; the label mapping lives
// here so an id this build doesn't know degrades to showing the number
// instead of a wrong name.

import type { MatchInfo } from '@/types'

/**
 * Majsoul ranked matchmode id → room tier key. From the game's own
 * matchmode table: each tier owns three 4p ids (best-of-one / East / South)
 * and two 3p ids (East / South); Melee has no best-of-one.
 */
const MAJSOUL_ROOM_TIERS: Record<number, string> = {
  1: 'bronze',
  2: 'bronze',
  3: 'bronze',
  4: 'silver',
  5: 'silver',
  6: 'silver',
  7: 'gold',
  8: 'gold',
  9: 'gold',
  10: 'jade',
  11: 'jade',
  12: 'jade',
  13: 'melee',
  14: 'melee',
  15: 'throne',
  16: 'throne',
  17: 'bronze',
  18: 'bronze',
  19: 'silver',
  20: 'silver',
  21: 'gold',
  22: 'gold',
  23: 'jade',
  24: 'jade',
  25: 'throne',
  26: 'throne',
  27: 'melee',
  28: 'melee',
}

/**
 * Tenhou `<GO type=…>` room bits → tier key. 0x80 alone = Joukyuu, 0x20
 * alone = Tokujou, both = Houou, neither = Ippan (ranked lobby 0 only —
 * private lobbies use the lobby number instead).
 */
function tenhouTier(goType: number): string {
  const idx = (goType & 0x20 ? 2 : 0) + (goType & 0x80 ? 1 : 0)
  return ['ippan', 'joukyuu', 'tokujou', 'houou'][idx]
}

/**
 * i18n key (+ params) for the room / rank lobby a game was played in, or
 * null when the record carries nothing displayable. Callers render with
 * `t(key, params)`.
 */
export function roomLabelKey(
  info: MatchInfo | null | undefined,
): { key: string; params?: Record<string, unknown> } | null {
  if (!info) return null
  switch (info.platform) {
    case 'majsoul': {
      if (info.mode_id == null) return null
      const tier = MAJSOUL_ROOM_TIERS[info.mode_id]
      return tier
        ? { key: `history.room.majsoul_${tier}` }
        : { key: 'history.room.raw', params: { id: info.mode_id } }
    }
    case 'tenhou': {
      if (info.lobby != null && info.lobby !== 0) {
        return { key: 'history.room.tenhou_lobby', params: { lobby: info.lobby } }
      }
      if (info.go_type == null) return null
      return { key: `history.room.tenhou_${tenhouTier(info.go_type)}` }
    }
    case 'riichi_city':
      return null
  }
}

/** The platform's own game (paifu) id, if the record carries one. */
export function matchGameId(info: MatchInfo | null | undefined): string | null {
  if (!info) return null
  switch (info.platform) {
    case 'majsoul':
      return info.game_uuid ?? null
    case 'tenhou':
      return info.log_id ?? null
    case 'riichi_city':
      return null
  }
}

/**
 * Replay URL, when one can be built without guessing. Tenhou log links are
 * region-independent; Majsoul replay hosts differ per region (which isn't
 * recorded), so Majsoul gets the copyable uuid only.
 */
export function paifuUrl(info: MatchInfo | null | undefined): string | null {
  if (info?.platform === 'tenhou' && info.log_id) {
    return `https://tenhou.net/0/?log=${encodeURIComponent(info.log_id)}`
  }
  return null
}
