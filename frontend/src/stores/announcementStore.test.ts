import { beforeEach, describe, expect, it, vi } from 'vitest'

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

const LAST_SEEN_KEY = 'akagi.announcement.releases.lastSeen'

function fixtureEntry(version: string) {
  return { version, date: '2026-01-01', features: [{ icon: () => null, key: 'x' }] }
}

// The store reads localStorage at module-init and bakes in the RELEASES
// data, so each test mocks the data module and re-imports a fresh copy.
async function freshStore(versions: string[]) {
  vi.resetModules()
  vi.doMock('@/announcements/releases', () => ({
    RELEASES: versions.map(fixtureEntry),
    releaseSlug: (v: string) => 'v' + v.replace(/[.-]/g, '_'),
  }))
  const mod = await import('./announcementStore')
  return mod.useAnnouncementStore
}

describe('announcementStore', () => {
  beforeEach(() => {
    stubLocalStorage()
  })

  it('arms the launch showing when the build is newer than the baseline', async () => {
    localStorage.setItem(LAST_SEEN_KEY, '3.5.0')
    const store = await freshStore(['3.6.0', '3.5.0'])

    expect(store.getState().prepareLaunch('3.6.0')).toBe(true)
    expect(store.getState().launchShowPending).toBe(true)
    expect(store.getState().open).toBe(false)
    expect(store.getState().entries.map((e) => e.version)).toEqual(['3.6.0'])

    store.getState().showLaunch()
    expect(store.getState().open).toBe(true)
  })

  it('replays skipped versions', async () => {
    localStorage.setItem(LAST_SEEN_KEY, '3.4.0')
    const store = await freshStore(['3.6.0', '3.5.0', '3.4.0', '3.3.0'])

    store.getState().prepareLaunch('3.6.0')
    expect(store.getState().entries.map((e) => e.version)).toEqual(['3.6.0', '3.5.0'])
  })

  it('does nothing when the baseline is current, and never opens un-armed', async () => {
    localStorage.setItem(LAST_SEEN_KEY, '3.6.0')
    const store = await freshStore(['3.6.0'])

    expect(store.getState().prepareLaunch('3.6.0')).toBe(false)
    expect(store.getState().launchShowPending).toBe(false)
    store.getState().showLaunch()
    expect(store.getState().open).toBe(false)
  })

  it('records the running version as seen on close, across restarts', async () => {
    const store = await freshStore(['3.6.0'])
    store.getState().prepareLaunch('3.6.0')
    store.getState().showLaunch()
    store.getState().close()

    expect(store.getState().open).toBe(false)
    expect(store.getState().launchShowPending).toBe(false)
    expect(localStorage.getItem(LAST_SEEN_KEY)).toBe('3.6.0')

    // "Restart": a fresh import must load the persisted baseline and
    // decide there is nothing left to show.
    const restarted = await freshStore(['3.6.0'])
    expect(restarted.getState().lastSeenVersion).toBe('3.6.0')
    expect(restarted.getState().prepareLaunch('3.6.0')).toBe(false)
  })

  it('never moves the baseline backwards on a downgraded build', async () => {
    localStorage.setItem(LAST_SEEN_KEY, '3.6.0')
    const store = await freshStore(['3.6.0', '3.5.0'])

    expect(store.getState().prepareLaunch('3.5.0')).toBe(false)
    store.getState().openHistory()
    store.getState().close()
    expect(localStorage.getItem(LAST_SEEN_KEY)).toBe('3.6.0')
  })

  it('history mode shows everything for the build, launch state untouched', async () => {
    localStorage.setItem(LAST_SEEN_KEY, '3.6.0')
    const store = await freshStore(['3.6.0', '3.5.0', '3.4.0'])
    store.getState().prepareLaunch('3.6.0')

    store.getState().openHistory()
    expect(store.getState().open).toBe(true)
    expect(store.getState().entries.map((e) => e.version)).toEqual([
      '3.6.0',
      '3.5.0',
      '3.4.0',
    ])
  })
})
