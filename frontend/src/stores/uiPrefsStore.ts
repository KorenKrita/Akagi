import { create } from 'zustand'

// Frontend-only UI preferences kept out of the Tauri-owned `AppConfig` since
// they don't affect any backend behavior. Sidebar collapsed/hover state lives
// in `useSidebar` (own zustand+persist store ported from shadcn-ui-sidebar).
const SCALE_KEY = 'akagi.ui.scale'

// One-time flag: has the user seen the dashboard onboarding hint (drag /
// resize / remove / add tiles)? Deliberately NOT reset by "Reset Layout" —
// this tracks "has seen the tutorial", not layout state.
const ONBOARDED_KEY = 'akagi.dashboard.onboarded'

// AkagiMS release announcement state. The dialog distinguishes intent:
// pressing "Got it" sets the dismissed flag for good, while closing via
// X / Esc / outside-click only counts a showing — the dialog returns on the
// next launch until it has been shown MAX_AKAGIMS_ANNOUNCEMENT_SHOWS times.
// The Overview promo card has its own independent dismissed flag.
const AKAGIMS_ANNOUNCEMENT_KEY = 'akagi.announcement.akagims'
const AKAGIMS_SHOWS_KEY = 'akagi.announcement.akagims.shows'
const AKAGIMS_CARD_KEY = 'akagi.announcement.akagims.card'

export const MAX_AKAGIMS_ANNOUNCEMENT_SHOWS = 3

export const SCALE_MIN = 0.7
export const SCALE_MAX = 1.5
export const SCALE_STEP = 0.05
export const SCALE_DEFAULT = 1.0

function clampScale(v: number): number {
  if (!Number.isFinite(v)) return SCALE_DEFAULT
  return Math.min(SCALE_MAX, Math.max(SCALE_MIN, v))
}

function loadScale(): number {
  if (typeof localStorage === 'undefined') return SCALE_DEFAULT
  try {
    const raw = localStorage.getItem(SCALE_KEY)
    if (!raw) return SCALE_DEFAULT
    return clampScale(parseFloat(raw))
  } catch {
    return SCALE_DEFAULT
  }
}

function loadFlag(key: string): boolean {
  if (typeof localStorage === 'undefined') return false
  try {
    return localStorage.getItem(key) === '1'
  } catch {
    return false
  }
}

function loadCount(key: string): number {
  if (typeof localStorage === 'undefined') return 0
  try {
    const n = parseInt(localStorage.getItem(key) ?? '0', 10)
    return Number.isFinite(n) && n > 0 ? n : 0
  } catch {
    return 0
  }
}

function storeFlag(key: string) {
  try {
    localStorage.setItem(key, '1')
  } catch {
    /* quota — ignore */
  }
}

type UiPrefsStore = {
  scale: number
  setScale: (v: number) => void
  resetScale: () => void
  /** Whether the dashboard onboarding hint has been dismissed at least once. */
  dashboardOnboarded: boolean
  markDashboardOnboarded: () => void
  /** Whether the AkagiMS announcement was explicitly dismissed ("Got it"). */
  akagimsAnnouncementDismissed: boolean
  markAkagimsAnnouncementDismissed: () => void
  /** How many times the AkagiMS announcement dialog has been shown. */
  akagimsAnnouncementShows: number
  recordAkagimsAnnouncementShown: () => void
  /** Whether the Overview AkagiMS promo card has been dismissed. */
  akagimsCardDismissed: boolean
  markAkagimsCardDismissed: () => void
}

export const useUiPrefsStore = create<UiPrefsStore>((set) => ({
  scale: loadScale(),
  setScale: (v) => {
    const scale = clampScale(v)
    try {
      localStorage.setItem(SCALE_KEY, String(scale))
    } catch {
      /* quota — ignore */
    }
    set({ scale })
  },
  resetScale: () => {
    try {
      localStorage.setItem(SCALE_KEY, String(SCALE_DEFAULT))
    } catch {
      /* quota — ignore */
    }
    set({ scale: SCALE_DEFAULT })
  },
  dashboardOnboarded: loadFlag(ONBOARDED_KEY),
  markDashboardOnboarded: () => {
    try {
      localStorage.setItem(ONBOARDED_KEY, '1')
    } catch {
      /* quota — ignore */
    }
    set({ dashboardOnboarded: true })
  },
  akagimsAnnouncementDismissed: loadFlag(AKAGIMS_ANNOUNCEMENT_KEY),
  markAkagimsAnnouncementDismissed: () => {
    storeFlag(AKAGIMS_ANNOUNCEMENT_KEY)
    set({ akagimsAnnouncementDismissed: true })
  },
  akagimsAnnouncementShows: loadCount(AKAGIMS_SHOWS_KEY),
  recordAkagimsAnnouncementShown: () => {
    set((s) => {
      const shows = s.akagimsAnnouncementShows + 1
      try {
        localStorage.setItem(AKAGIMS_SHOWS_KEY, String(shows))
      } catch {
        /* quota — ignore */
      }
      return { akagimsAnnouncementShows: shows }
    })
  },
  akagimsCardDismissed: loadFlag(AKAGIMS_CARD_KEY),
  markAkagimsCardDismissed: () => {
    storeFlag(AKAGIMS_CARD_KEY)
    set({ akagimsCardDismissed: true })
  },
}))
