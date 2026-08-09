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
  return mod
}

describe('uiPrefsStore AkagiMS announcement state', () => {
  beforeEach(() => {
    stubLocalStorage()
  })

  it('defaults to not dismissed and zero showings on first launch', async () => {
    const { useUiPrefsStore } = await freshStore()
    expect(useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(false)
    expect(useUiPrefsStore.getState().akagimsAnnouncementShows).toBe(0)
    expect(useUiPrefsStore.getState().akagimsCardDismissed).toBe(false)
  })

  it('persists explicit dismissal ("Got it") across restarts', async () => {
    const { useUiPrefsStore } = await freshStore()
    useUiPrefsStore.getState().markAkagimsAnnouncementDismissed()
    expect(localStorage.getItem('akagi.announcement.akagims')).toBe('1')

    const restarted = await freshStore()
    expect(restarted.useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(true)
  })

  it('counts showings and persists the count across restarts', async () => {
    const { useUiPrefsStore, MAX_AKAGIMS_ANNOUNCEMENT_SHOWS } = await freshStore()
    useUiPrefsStore.getState().recordAkagimsAnnouncementShown()
    useUiPrefsStore.getState().recordAkagimsAnnouncementShown()
    expect(useUiPrefsStore.getState().akagimsAnnouncementShows).toBe(2)
    expect(localStorage.getItem('akagi.announcement.akagims.shows')).toBe('2')

    // A soft close (X / Esc) never sets the dismissed flag — the dialog is
    // eligible again after restart until the cap is reached.
    const restarted = await freshStore()
    const s = restarted.useUiPrefsStore.getState()
    expect(s.akagimsAnnouncementShows).toBe(2)
    expect(s.akagimsAnnouncementDismissed).toBe(false)
    expect(s.akagimsAnnouncementShows < MAX_AKAGIMS_ANNOUNCEMENT_SHOWS).toBe(true)

    s.recordAkagimsAnnouncementShown()
    expect(
      restarted.useUiPrefsStore.getState().akagimsAnnouncementShows >=
        MAX_AKAGIMS_ANNOUNCEMENT_SHOWS,
    ).toBe(true)
  })

  it('recovers from a corrupt showing count', async () => {
    localStorage.setItem('akagi.announcement.akagims.shows', 'banana')
    const { useUiPrefsStore } = await freshStore()
    expect(useUiPrefsStore.getState().akagimsAnnouncementShows).toBe(0)
  })

  it('keeps the Overview card flag independent of the dialog', async () => {
    const { useUiPrefsStore } = await freshStore()
    useUiPrefsStore.getState().markAkagimsCardDismissed()
    expect(useUiPrefsStore.getState().akagimsCardDismissed).toBe(true)
    expect(useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(false)

    const restarted = await freshStore()
    expect(restarted.useUiPrefsStore.getState().akagimsCardDismissed).toBe(true)
    expect(restarted.useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(false)
  })

  it('keeps the announcement flag independent of dashboard onboarding', async () => {
    const { useUiPrefsStore } = await freshStore()
    useUiPrefsStore.getState().markDashboardOnboarded()
    expect(useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(false)

    const restarted = await freshStore()
    expect(restarted.useUiPrefsStore.getState().dashboardOnboarded).toBe(true)
    expect(restarted.useUiPrefsStore.getState().akagimsAnnouncementDismissed).toBe(false)
  })
})
