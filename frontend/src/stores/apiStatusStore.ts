import { create } from 'zustand'

/**
 * Runtime health of the built-in bot's online inference API, driven by the
 * backend's `native-api-health` notifications: the native bot toasts a warning
 * when the server goes down, a success when it recovers, and an info when the
 * user turns cloud inference on or off mid-game. Only `warn`/`error` mean
 * degraded, so the on/off toasts clear the indicator. The Statusbar reads this
 * to colour the "Online API" LED. Reset at each `start_game` so a stale outage
 * from a previous game doesn't carry over.
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
