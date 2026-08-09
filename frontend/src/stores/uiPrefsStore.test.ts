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

// The store reads localStorage at module-init, so each test re-imports a
// fresh copy after seeding storage to exercise the load path.
async function freshStore() {
  vi.resetModules()
  const mod = await import('./uiPrefsStore')
  return mod.useUiPrefsStore
}

describe('uiPrefsStore AkagiMS announcement flag', () => {
  beforeEach(() => {
    stubLocalStorage()
  })

  it('defaults to unseen so the announcement shows on first launch', async () => {
    const store = await freshStore()
    expect(store.getState().akagimsAnnouncementSeen).toBe(false)
  })

  it('persists dismissal so the announcement never reappears', async () => {
    const store = await freshStore()
    store.getState().markAkagimsAnnouncementSeen()
    expect(store.getState().akagimsAnnouncementSeen).toBe(true)
    expect(localStorage.getItem('akagi.announcement.akagims')).toBe('1')

    // Simulate an app restart: a fresh module init must read the flag back.
    const restarted = await freshStore()
    expect(restarted.getState().akagimsAnnouncementSeen).toBe(true)
  })

  it('keeps the announcement flag independent of dashboard onboarding', async () => {
    const store = await freshStore()
    store.getState().markDashboardOnboarded()
    expect(store.getState().akagimsAnnouncementSeen).toBe(false)

    const restarted = await freshStore()
    expect(restarted.getState().dashboardOnboarded).toBe(true)
    expect(restarted.getState().akagimsAnnouncementSeen).toBe(false)
  })
})
