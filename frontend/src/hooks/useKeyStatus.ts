import { useEffect, useState } from 'react'
import { invoke } from '@/lib/tauri'
import type { KeyStatus } from '@/types'

/**
 * Live status of the saved MJOT key (`GET /v3/key`), or `null` while unknown.
 *
 * `null` covers every "don't build UI on this key" case at once: no key
 * typed, no server URL, fetch still in flight, and — because an expired or
 * disabled key answers 401 — a key that is no longer usable. The purchase
 * and redeem dialogs lean on exactly that: their "add time to my current
 * key" default flips on only when this returns a status.
 *
 * Purely advisory — errors are swallowed, never surfaced. The answer is
 * tagged with the key it was fetched for and filtered on read, so a stale
 * response for a previously-typed key reads as `null` instead of needing a
 * state reset in the effect (which would cascade renders).
 */
export function useKeyStatus(baseUrl: string, proxy: string, key: string): KeyStatus | null {
  const [statusFor, setStatusFor] = useState<{ key: string; st: KeyStatus } | null>(null)
  const trimmed = key.trim()
  useEffect(() => {
    if (trimmed === '' || baseUrl.trim() === '') return
    let cancelled = false
    void invoke<KeyStatus>('native_api_key_status', { baseUrl, proxy, key: trimmed })
      .then((st) => {
        if (!cancelled) setStatusFor({ key: trimmed, st })
      })
      .catch(() => {
        /* invalid or expired key — callers see null */
      })
    return () => {
      cancelled = true
    }
  }, [baseUrl, proxy, trimmed])
  return statusFor !== null && statusFor.key === trimmed ? statusFor.st : null
}
