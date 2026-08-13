import { create } from 'zustand'

import { ANNOUNCEMENTS, type AnnouncementEntry } from '@/announcements/entries'
import { eligibleEntries, selectUnseenEntries } from '@/announcements/select'

// ISO date of the newest announcement the user has already seen. Absent
// on fresh installs and on the first run of a build that introduced the
// system — `selectUnseenEntries` treats that as "show the newest few".
const LAST_SEEN_KEY = 'akagi.announcement.lastSeen'

function loadLastSeen(): string | null {
  if (typeof localStorage === 'undefined') return null
  try {
    return localStorage.getItem(LAST_SEEN_KEY)
  } catch {
    return null
  }
}

type AnnouncementStore = {
  /** Persisted baseline; null until the user closes their first showing. */
  lastSeenDate: string | null
  /** Version resolved at launch by `<AnnouncementsDialog />`. */
  currentVersion: string | null
  /** Entries the open dialog is showing, newest first. */
  entries: AnnouncementEntry[]
  open: boolean
  /** True while an armed launch showing hasn't been opened yet. */
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
  /** Any close (Got it / X / Esc / outside click) marks the shown entries seen. */
  close: () => void
}

export const useAnnouncementStore = create<AnnouncementStore>((set, get) => ({
  lastSeenDate: loadLastSeen(),
  currentVersion: null,
  entries: [],
  open: false,
  launchShowPending: false,

  prepareLaunch: (currentVersion) => {
    const unseen = selectUnseenEntries(ANNOUNCEMENTS, get().lastSeenDate, currentVersion)
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
    const entries =
      current === null ? [...ANNOUNCEMENTS] : eligibleEntries(ANNOUNCEMENTS, current)
    set({ entries, open: true })
  },

  close: () => {
    set((s) => {
      // The newest shown entry's date becomes the baseline. Only ever
      // advance — closing the dialog on a downgraded build (which shows
      // older entries) must not resurrect announcements the user already
      // saw on the newer one.
      const newest = s.entries[0]?.date ?? null
      const advance =
        newest !== null && (s.lastSeenDate === null || newest > s.lastSeenDate)
      if (advance) {
        try {
          localStorage.setItem(LAST_SEEN_KEY, newest)
        } catch {
          /* quota — ignore */
        }
      }
      return {
        open: false,
        launchShowPending: false,
        lastSeenDate: advance ? newest : s.lastSeenDate,
      }
    })
  },
}))
