import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { GameRecord } from '@/types'

// Node 22+ ships an experimental `localStorage` global that is undefined
// without `--localstorage-file` and shadows jsdom's, so install an explicit
// in-memory stand-in the store and the assertions both see.
function stubLocalStorage() {
  const backing = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => backing.get(k) ?? null,
    setItem: (k: string, v: string) => void backing.set(k, String(v)),
    removeItem: (k: string) => void backing.delete(k),
    clear: () => backing.clear(),
  })
  return backing
}

// Persisted mirrors are namespaced by the active key's last 4 chars — the
// mocked config key is 'k'.repeat(32), so the suffix is 'kkkk'.
const GAME_MAP_KEY = 'akagi.review.gameMap.kkkk'
const PENDING_JOB_KEY = 'akagi.review.pendingJob.kkkk'

const record = (id: string, numPlayers = 4) =>
  ({ id, num_players: numPlayers, our_seat: 0 }) as unknown as GameRecord

// The store reads localStorage lazily (per-key hydration) and keeps its poll
// timer at module scope, so each test mocks the dependencies and re-imports
// a fresh copy.
async function freshStore(invoke: ReturnType<typeof vi.fn>) {
  vi.resetModules()
  vi.doMock('@/lib/tauri', () => ({
    HAS_TAURI: true,
    invoke,
    listen: async () => () => {},
  }))
  vi.doMock('@/lib/external', () => ({ openExternal: vi.fn() }))
  vi.doMock('i18next', () => ({ default: { t: (k: string) => k } }))
  vi.doMock('@/components/ui/sonner', () => ({
    toast: {
      info: vi.fn(),
      success: vi.fn(),
      warning: vi.fn(),
      error: vi.fn(),
      dismiss: vi.fn(),
    },
  }))
  vi.doMock('sonner', () => ({
    toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }),
  }))
  vi.doMock('@/stores/configStore', () => ({
    useConfigStore: {
      getState: () => ({
        config: {
          bot: {
            api: {
              enabled: false, // review must not depend on the live-inference toggle
              base_url: 'https://srv',
              key: 'k'.repeat(32),
              model_4p: '4p-x',
              model_3p: '',
              proxy_enabled: false,
              proxy: 'socks5://ignored-while-disabled',
              react_timeout_ms: 3000,
            },
          },
        },
      }),
    },
  }))
  const mod = await import('./reviewStore')
  return mod.useReviewStore
}

describe('reviewStore', () => {
  beforeEach(() => {
    stubLocalStorage()
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('submit maps the game, persists per-key, and passes the configured model', async () => {
    const invoke = vi.fn().mockResolvedValue({ review_id: 'r1', status: 'queued' })
    const store = await freshStore(invoke)

    await store.getState().submit(record('GAME1'))

    expect(invoke).toHaveBeenCalledWith('native_api_review_history_game', {
      baseUrl: 'https://srv',
      proxy: '', // proxy_enabled=false ⇒ direct, even with a proxy typed in
      key: 'k'.repeat(32),
      id: 'GAME1',
      model: '4p-x',
    })
    expect(store.getState().job).toMatchObject({
      reviewId: 'r1',
      historyId: 'GAME1',
      status: 'queued',
    })
    expect(store.getState().gameMap).toEqual({ GAME1: 'r1' })
    // Persisted under the key-scoped names, so a key switch can't cross wires.
    expect(JSON.parse(localStorage.getItem(GAME_MAP_KEY)!)).toEqual({
      GAME1: 'r1',
    })
    expect(JSON.parse(localStorage.getItem(PENDING_JOB_KEY)!)).toMatchObject({
      reviewId: 'r1',
    })
  })

  it('refuses a second submit while a job is active (server allows one)', async () => {
    const invoke = vi.fn().mockResolvedValue({ review_id: 'r1', status: 'queued' })
    const store = await freshStore(invoke)

    await store.getState().submit(record('GAME1'))
    await store.getState().submit(record('GAME2'))

    expect(
      invoke.mock.calls.filter(
        ([cmd]) => cmd === 'native_api_review_history_game',
      ),
    ).toHaveLength(1)
  })

  it('resume hydrates the persisted per-key job and mapping', async () => {
    localStorage.setItem(GAME_MAP_KEY, JSON.stringify({ GAME1: 'r1' }))
    localStorage.setItem(
      PENDING_JOB_KEY,
      JSON.stringify({
        reviewId: 'r1',
        historyId: 'GAME1',
        status: 'queued',
        progress: 0,
      }),
    )
    const invoke = vi.fn().mockResolvedValue({ status: 'running', progress: 0.5 })
    const store = await freshStore(invoke)

    store.getState().resume()
    expect(store.getState().gameMap).toEqual({ GAME1: 'r1' })
    expect(store.getState().job?.reviewId).toBe('r1')

    await vi.advanceTimersByTimeAsync(0) // hydration poll fires immediately
    expect(store.getState().job).toMatchObject({
      status: 'running',
      progress: 0.5,
    })
  })

  it('a done poll clears the job, caches the share URL, and reloads shares', async () => {
    const invoke = vi.fn((cmd: string) => {
      switch (cmd) {
        case 'native_api_review_history_game':
          return Promise.resolve({ review_id: 'r1', status: 'queued' })
        case 'native_api_review_status':
          return Promise.resolve({
            status: 'done',
            share_id: 'sh1',
            url: 'https://viewer/preview/s/sh1',
          })
        case 'native_api_list_shares':
          return Promise.resolve([])
        default:
          return Promise.reject(new Error(`unexpected ${cmd}`))
      }
    })
    const store = await freshStore(invoke)

    await store.getState().submit(record('GAME1'))
    await vi.advanceTimersByTimeAsync(5000) // first poll fires

    expect(store.getState().job).toBeNull()
    expect(localStorage.getItem(PENDING_JOB_KEY)).toBeNull()
    expect(store.getState().shareUrls).toEqual({
      sh1: 'https://viewer/preview/s/sh1',
    })
    expect(invoke).toHaveBeenCalledWith(
      'native_api_list_shares',
      expect.objectContaining({ baseUrl: 'https://srv' }),
    )
    // The mapping survives the job — it labels the game as reviewed.
    expect(store.getState().gameMap).toEqual({ GAME1: 'r1' })
  })

  it('a failed poll ends the job without unmapping the game', async () => {
    const invoke = vi.fn((cmd: string) => {
      switch (cmd) {
        case 'native_api_review_history_game':
          return Promise.resolve({ review_id: 'r1', status: 'queued' })
        case 'native_api_review_status':
          return Promise.resolve({ status: 'failed', error: 'server busy' })
        case 'native_api_list_shares':
          return Promise.resolve([])
        default:
          return Promise.reject(new Error(`unexpected ${cmd}`))
      }
    })
    const store = await freshStore(invoke)

    await store.getState().submit(record('GAME1'))
    await vi.advanceTimersByTimeAsync(5000)

    expect(store.getState().job).toBeNull()
    expect(localStorage.getItem(PENDING_JOB_KEY)).toBeNull()
  })

  it('a real HTTP 404 on the poll is terminal (job dropped)', async () => {
    localStorage.setItem(
      PENDING_JOB_KEY,
      JSON.stringify({
        reviewId: 'r1',
        historyId: 'GAME1',
        status: 'queued',
        progress: 0,
      }),
    )
    const invoke = vi
      .fn()
      .mockRejectedValue(new Error('review status failed: HTTP 404 — not found'))
    const store = await freshStore(invoke)

    store.getState().resume()
    await vi.advanceTimersByTimeAsync(0)

    expect(store.getState().job).toBeNull()
    expect(localStorage.getItem(PENDING_JOB_KEY)).toBeNull()
  })

  it("a transport error whose URL merely contains '404' is NOT terminal", async () => {
    // reqwest appends the request URL to transport errors; a hex review id
    // or a port like :4041 can contain the digits 404. That must back off,
    // not kill the persisted job.
    const invoke = vi.fn((cmd: string) => {
      switch (cmd) {
        case 'native_api_review_history_game':
          return Promise.resolve({ review_id: 'ab404c', status: 'queued' })
        case 'native_api_review_status':
          return Promise.reject(
            new Error(
              'GET /v3/review/<id>: error sending request for url (https://srv:4041/v3/review/ab404c)',
            ),
          )
        default:
          return Promise.reject(new Error(`unexpected ${cmd}`))
      }
    })
    const store = await freshStore(invoke)

    await store.getState().submit(record('GAME1'))
    await vi.advanceTimersByTimeAsync(5000) // poll 1 errors → backoff 10s
    expect(store.getState().job).not.toBeNull()
    await vi.advanceTimersByTimeAsync(10_000) // poll 2 fires after backoff
    expect(
      invoke.mock.calls.filter(([cmd]) => cmd === 'native_api_review_status'),
    ).toHaveLength(2)
    expect(store.getState().job).not.toBeNull()
    expect(localStorage.getItem(PENDING_JOB_KEY)).not.toBeNull()
  })

  it('reshare drops the stale mapping only on a real 404', async () => {
    localStorage.setItem(GAME_MAP_KEY, JSON.stringify({ GAME1: 'r1' }))
    const invoke = vi
      .fn()
      .mockRejectedValue(new Error('review share failed: HTTP 404 — not found'))
    const store = await freshStore(invoke)

    const url = await store.getState().reshare('GAME1')

    expect(url).toBeNull()
    expect(store.getState().gameMap).toEqual({})
    expect(localStorage.getItem(GAME_MAP_KEY)).toBe('{}')
  })

  it('reshare keeps the mapping on a non-404 failure', async () => {
    localStorage.setItem(GAME_MAP_KEY, JSON.stringify({ GAME1: 'r1' }))
    const invoke = vi
      .fn()
      .mockRejectedValue(new Error('review share failed: HTTP 503 — busy'))
    const store = await freshStore(invoke)

    const url = await store.getState().reshare('GAME1')

    expect(url).toBeNull()
    expect(store.getState().gameMap).toEqual({ GAME1: 'r1' })
  })
})
