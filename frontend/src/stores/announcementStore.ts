import { create } from 'zustand'

import { RELEASES, type ReleaseEntry } from '@/announcements/releases'
import { releasedEntries, selectUnseenReleases } from '@/announcements/select'
import { compareVersions } from '@/lib/appVersion'

// Highest version whose release announcement the user has already seen.
// Absent on fresh installs and on the first run of a build that
// introduced the system — `selectUnseenReleases` treats that as "show
// the newest few".
const LAST_SEEN_KEY = 'akagi.announcement.releases.lastSeen'

function loadLastSeen(): string | null {
  if (typeof localStorage === 'undefined') return null
  try {
    return localStorage.getItem(LAST_SEEN_KEY)
  } catch {
    return null
  }
}

type AnnouncementStore = {
  /** Persisted baseline; null until the user closes their first What's-new. */
  lastSeenVersion: string | null
  /** Version resolved at launch by `<ReleaseAnnouncementDialog />`. */
  currentVersion: string | null
  /** Entries the open dialog is showing. */
  entries: ReleaseEntry[]
  open: boolean
  /**
   * True from the moment the launch check decides the dialog will show
   * until that showing is closed. The AkagiMS promo dialog reads this to
   * defer (without spending one of its capped showings) so the two never
   * stack on one launch.
   */
  launchShowPending: boolean
  /**
   * Launch path, called once the running version resolves: pick the
   * unseen entries and arm the dialog. Returns whether there is anything
   * to show; the caller opens it after its own grace delay.
   */
  prepareLaunch: (currentVersion: string) => boolean
  /** Actually open the armed launch showing (after the caller's delay). */
  showLaunch: () => void
  /** Settings entry point: browse every announcement for this build. */
  openHistory: () => void
  /** Any close (Got it / X / Esc / outside click) marks the build seen. */
  close: () => void
}

export const useAnnouncementStore = create<AnnouncementStore>((set, get) => ({
  lastSeenVersion: loadLastSeen(),
  currentVersion: null,
  entries: [],
  open: false,
  launchShowPending: false,

  prepareLaunch: (currentVersion) => {
    const unseen = selectUnseenReleases(RELEASES, get().lastSeenVersion, currentVersion)
    if (unseen.length === 0) {
      set({ currentVersion })
      return false
    }
    set({ currentVersion, entries: unseen, launchShowPending: true })
    return true
  },

  showLaunch: () => {
    if (get().launchShowPending) set({ open: true })
  },

  openHistory: () => {
    const current = get().currentVersion
    const entries = current === null ? [...RELEASES] : releasedEntries(RELEASES, current)
    set({ entries, open: true })
  },

  close: () => {
    set((s) => {
      // Only ever advance the baseline — closing the dialog on a
      // downgraded build must not resurrect announcements the user
      // already saw on the newer one.
      const version = s.currentVersion
      const advance =
        version !== null &&
        (s.lastSeenVersion === null || compareVersions(version, s.lastSeenVersion) > 0)
      if (advance) {
        try {
          localStorage.setItem(LAST_SEEN_KEY, version)
        } catch {
          /* quota — ignore */
        }
      }
      return {
        open: false,
        launchShowPending: false,
        lastSeenVersion: advance ? version : s.lastSeenVersion,
      }
    })
  },
}))
