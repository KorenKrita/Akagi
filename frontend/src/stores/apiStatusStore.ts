import { create } from 'zustand'

/**
 * Runtime health of the built-in bot's online inference API, driven by the
 * backend's `native-api-health` notifications (the `ApiNativeBot` toasts a
 * warning when the server goes down and a success when it recovers). The
 * Statusbar reads this to colour the "Online API" indicator. Reset at each
 * `start_game` so a stale outage from a previous game doesn't carry over.
 */
type ApiStatusStore = {
  degraded: boolean
  error?: string
  setDegraded: (degraded: boolean, error?: string) => void
  reset: () => void
}

export const useApiStatusStore = create<ApiStatusStore>((set) => ({
  degraded: false,
  error: undefined,
  setDegraded: (degraded, error) => set({ degraded, error: degraded ? error : undefined }),
  reset: () => set({ degraded: false, error: undefined }),
}))
