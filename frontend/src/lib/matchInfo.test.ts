import { describe, expect, it } from 'vitest'

import { matchGameId, paifuUrl, roomLabelKey } from './matchInfo'

describe('roomLabelKey', () => {
  it('maps Majsoul ranked mode ids to room tiers', () => {
    expect(
      roomLabelKey({ platform: 'majsoul', mode_id: 9 }),
    ).toEqual({ key: 'history.room.majsoul_gold' })
    // 3p ids share the tier keys.
    expect(
      roomLabelKey({ platform: 'majsoul', mode_id: 17 }),
    ).toEqual({ key: 'history.room.majsoul_bronze' })
    expect(
      roomLabelKey({ platform: 'majsoul', mode_id: 26 }),
    ).toEqual({ key: 'history.room.majsoul_throne' })
  })

  it('falls back to the raw number for unknown Majsoul mode ids', () => {
    expect(roomLabelKey({ platform: 'majsoul', mode_id: 99 })).toEqual({
      key: 'history.room.raw',
      params: { id: 99 },
    })
    expect(roomLabelKey({ platform: 'majsoul' })).toBeNull()
  })

  it('decodes Tenhou GO type bits into room tiers', () => {
    // 0x80 alone = Joukyuu, 0x20 alone = Tokujou, both = Houou.
    expect(
      roomLabelKey({ platform: 'tenhou', go_type: 0x09, lobby: 0 }),
    ).toEqual({ key: 'history.room.tenhou_ippan' })
    expect(
      roomLabelKey({ platform: 'tenhou', go_type: 0x89, lobby: 0 }),
    ).toEqual({ key: 'history.room.tenhou_joukyuu' })
    expect(
      roomLabelKey({ platform: 'tenhou', go_type: 0x29, lobby: 0 }),
    ).toEqual({ key: 'history.room.tenhou_tokujou' })
    expect(
      roomLabelKey({ platform: 'tenhou', go_type: 0xa9, lobby: 0 }),
    ).toEqual({ key: 'history.room.tenhou_houou' })
  })

  it('labels non-zero Tenhou lobbies by number instead of tier', () => {
    expect(
      roomLabelKey({ platform: 'tenhou', go_type: 0x09, lobby: 7994 }),
    ).toEqual({ key: 'history.room.tenhou_lobby', params: { lobby: 7994 } })
  })

  it('maps Riichi City stage types 1-4 to Star/Moon/Sun/Galaxy', () => {
    expect(
      roomLabelKey({ platform: 'riichi_city', stage_type: 1 }),
    ).toEqual({ key: 'history.room.rc_star' })
    expect(
      roomLabelKey({ platform: 'riichi_city', stage_type: 4 }),
    ).toEqual({ key: 'history.room.rc_galaxy' })
    // 0/absent = not a ranked queue; unknown values degrade to the number.
    expect(roomLabelKey({ platform: 'riichi_city', stage_type: 0 })).toBeNull()
    expect(roomLabelKey({ platform: 'riichi_city' })).toBeNull()
    expect(roomLabelKey({ platform: 'riichi_city', stage_type: 9 })).toEqual({
      key: 'history.room.raw',
      params: { id: 9 },
    })
  })

  it('returns null for missing match info', () => {
    expect(roomLabelKey(null)).toBeNull()
    expect(roomLabelKey(undefined)).toBeNull()
  })
})

describe('matchGameId / paifuUrl', () => {
  it('surfaces each platform game id', () => {
    expect(
      matchGameId({ platform: 'majsoul', game_uuid: '240101-uuid' }),
    ).toBe('240101-uuid')
    expect(
      matchGameId({ platform: 'tenhou', log_id: '2026010100gm-00a9-0000-cafe0001' }),
    ).toBe('2026010100gm-00a9-0000-cafe0001')
    expect(
      matchGameId({ platform: 'riichi_city', room_id: 'tabletoken0001' }),
    ).toBe('tabletoken0001')
  })

  it('builds replay links for Tenhou only', () => {
    expect(
      paifuUrl({ platform: 'tenhou', log_id: '2026010100gm-00a9-0000-cafe0001' }),
    ).toBe('https://tenhou.net/0/?log=2026010100gm-00a9-0000-cafe0001')
    expect(paifuUrl({ platform: 'majsoul', game_uuid: 'u' })).toBeNull()
    expect(paifuUrl({ platform: 'riichi_city', room_id: 'x' })).toBeNull()
  })
})
