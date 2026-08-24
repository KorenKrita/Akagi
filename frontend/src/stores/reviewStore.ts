// Whole-game review store (Beta). Drives the `/v3/review` background-job
// lifecycle against the inference server:
//
//   submit a recorded history game → poll the job every few seconds →
//   done ⇒ the review's public share URL (the MJOT viewer page).
//
// This lives in a store (not page state) so a running job survives the
// Review page unmounting — the poll keeps going app-wide and the toast on
// completion fires wherever the user is. Server-side constraints the UI
// must respect (all enforced as 429s with Retry-After):
//   - one accepted submit per key per 10 minutes,
//   - one queued/running job per key at a time,
//   - `reviews_per_day` per UTC day (0 ⇒ the plan has no review access).
//
// The server knows jobs and shares but not which *local* history record a
// job came from, so `gameMap` (historyId → reviewId) is persisted to
// localStorage; joining it against the live share list labels history rows
// as already-reviewed. A pending job is persisted too, so a review queued
// just before the app closed is picked up again on the next launch.
//
// Jobs and shares are **per API key** on the server, so both persisted
// mirrors are namespaced by the active key (see `ensureHydrated`): an
// unscoped map would keep polling another key's review ids after a key
// switch — guaranteed 404s and misleading "job lost" toasts.

import { create } from 'zustand'
import i18n from 'i18next'
// The raw sonner toast, not the ui wrapper: the "review done" toast carries
// an Open action button, which the wrapper's narrow options don't expose.
import { toast as rawToast } from 'sonner'
import { invoke } from '@/lib/tauri'
import { openExternal } from '@/lib/external'
import { toast } from '@/components/ui/sonner'
import { useConfigStore } from '@/stores/configStore'
import type {
  GameRecord,
  ReviewJobStatus,
  ReviewSubmitted,
  ShareEntry,
  ShareIssued,
} from '@/types'

/** Poll cadence for `GET /v3/review/{id}`. The server asks for 4-5 s; polls
 *  draw on a separate per-key bucket (20/min, burst 40) shared with the
 *  share list/revoke calls, so 5 s (12/min) leaves room for those. */
const POLL_MS = 5000
/** Transient-error backoff cap. The job lives server-side, so polling never
 *  gives up — it just slows down while the network is unhappy. */
const POLL_BACKOFF_MAX_MS = 60_000

const GAME_MAP_PREFIX = 'akagi.review.gameMap'
const PENDING_JOB_PREFIX = 'akagi.review.pendingJob'

export type ActiveJob = {
  reviewId: string
  /** The local history record this job reviews; null when unknown. */
  historyId: string | null
  status: 'queued' | 'running'
  /** 0..=1. */
  progress: number
}

function loadJson<T>(key: string): T | null {
  if (typeof localStorage === 'undefined') return null
  try {
    const raw = localStorage.getItem(key)
    return raw ? (JSON.parse(raw) as T) : null
  } catch {
    return null
  }
}

function saveJson(key: string, value: unknown | null) {
  if (typeof localStorage === 'undefined') return
  try {
    if (value === null) localStorage.removeItem(key)
    else localStorage.setItem(key, JSON.stringify(value))
  } catch {
    /* quota — ignore */
  }
}

/** The saved `bot.api` connection values, or null when no key is configured.
 *  Mirrors the backend's `effective_proxy`: the proxy applies only while its
 *  toggle is on. Review deliberately does NOT check `api.enabled` — that
 *  switch routes *live decisions* through the cloud; reviewing past games
 *  with a valid key is useful either way. */
export function reviewApiCfg(): { baseUrl: string; proxy: string; key: string } | null {
  const cfg = useConfigStore.getState().config
  if (!cfg) return null
  const api = cfg.bot.api
  const baseUrl = api.base_url.trim()
  const key = api.key.trim()
  if (baseUrl === '' || key === '') return null
  return { baseUrl, proxy: api.proxy_enabled ? api.proxy.trim() : '', key }
}

/** True when an IPC error string is a genuine server 404 — matched on the
 *  exact "<what> failed: HTTP 404" shape the Rust client's `check()` emits.
 *  A bare `includes('404')` would misfire on transport errors, which carry
 *  the full request URL (reqwest appends "for url (…)"): `404` can appear in
 *  a hex review id or a self-hosted base_url port, and treating those as
 *  terminal would delete a job/mapping that is actually alive. */
const isHttp404 = (msg: string) => /\bfailed: HTTP 404\b/.test(msg)

// Poll machinery lives outside the store: implementation detail, and keeping
// it out of state avoids re-renders on every reschedule.
let pollTimer: ReturnType<typeof setTimeout> | null = null
let pollErrors = 0
let pollInFlight = false
/** Storage namespace currently hydrated — the active key's last 4 chars
 *  (the same label the server itself uses as `key_last4`). */
let storageOwner: string | null = null

const gameMapKey = () => `${GAME_MAP_PREFIX}.${storageOwner ?? 'anon'}`
const pendingJobKey = () => `${PENDING_JOB_PREFIX}.${storageOwner ?? 'anon'}`

type ReviewStore = {
  /** Live share links from `GET /v3/shares`; null until first load. */
  shares: ShareEntry[] | null
  sharesLoading: boolean
  sharesError: string | null

  /** The queued/running job, if any. The server allows at most one. */
  job: ActiveJob | null
  submitting: boolean

  /** historyId → reviewId for every submit made from this install (with the
   *  active key). Hydrated per key by `ensureHydrated`. */
  gameMap: Record<string, string>

  /** shareId → viewer URL, resolved lazily. The share listing carries ids
   *  only; the URL comes from the job poll (or a share re-issue). */
  shareUrls: Record<string, string>

  loadShares: () => Promise<void>
  /** The viewer URL for a listed share, fetched on first use and cached.
   *  Null when it can't be resolved (network, job evicted). */
  resolveShareUrl: (share: ShareEntry) => Promise<string | null>
  /** Submit a recorded game for review. Refuses while a job is active.
   *  `model` overrides the configured model id ('' ⇒ server default);
   *  omitted ⇒ the configured model for the game's player count.
   *  Resolves true when the job was accepted. */
  submit: (record: GameRecord, model?: string) => Promise<boolean>
  /** Hydrate the active key's persisted state and restart polling for a
   *  persisted pending job (app relaunch, page mount, key switch). */
  resume: () => void
  /** Revoke a share link. The review stays stored server-side. */
  revoke: (shareId: string) => Promise<void>
  /** Re-issue the share link for an already-reviewed game (after a revoke).
   *  Returns the fresh URL, or null when the review no longer exists —
   *  in that case the game is unmarked so it can be reviewed again. */
  reshare: (historyId: string) => Promise<string | null>
}

export const useReviewStore = create<ReviewStore>((set, get) => {
  const schedulePoll = (delay: number) => {
    if (pollTimer !== null) clearTimeout(pollTimer)
    pollTimer = setTimeout(() => {
      pollTimer = null
      void pollOnce()
    }, delay)
  }

  const stopPolling = () => {
    if (pollTimer !== null) clearTimeout(pollTimer)
    pollTimer = null
    pollErrors = 0
  }

  const finishJob = () => {
    stopPolling()
    saveJson(pendingJobKey(), null)
    set({ job: null })
  }

  /** Point the persisted mirrors at the active key's namespace. On a key
   *  change this drops the old key's in-memory state (its slot stays in
   *  localStorage for a switch back), loads the new slot, and restarts the
   *  poll for a pending job found there. */
  const ensureHydrated = () => {
    const api = reviewApiCfg()
    if (!api) return
    const owner = api.key.slice(-4)
    if (owner === storageOwner) return
    storageOwner = owner
    stopPolling()
    const job = loadJson<ActiveJob>(pendingJobKey())
    set({
      job,
      gameMap: loadJson<Record<string, string>>(gameMapKey()) ?? {},
      // The share list and URL cache belong to the previous key.
      shares: null,
      sharesError: null,
      shareUrls: {},
    })
    if (job) schedulePoll(0)
  }

  const pollOnce = async () => {
    if (pollInFlight) return
    const job = get().job
    if (!job) return
    const api = reviewApiCfg()
    if (!api) {
      // Key removed mid-job: keep the job persisted, stop hammering.
      schedulePoll(POLL_BACKOFF_MAX_MS)
      return
    }
    pollInFlight = true
    let st: ReviewJobStatus
    try {
      st = await invoke<ReviewJobStatus>('native_api_review_status', {
        baseUrl: api.baseUrl,
        proxy: api.proxy,
        key: api.key,
        reviewId: job.reviewId,
      })
    } catch (e) {
      // The job may have changed while we were awaiting (key switch, a
      // fresh submit): this response belongs to a job we no longer track.
      if (get().job?.reviewId !== job.reviewId) return
      // A genuine 404 is terminal: unknown/evicted job (or another key's).
      // Anything else — timeouts, 429s, restarts — is worth waiting out.
      if (isHttp404(String(e))) {
        finishJob()
        toast.error(i18n.t('review.job_lost'))
        return
      }
      pollErrors += 1
      schedulePoll(Math.min(POLL_MS * 2 ** pollErrors, POLL_BACKOFF_MAX_MS))
      return
    } finally {
      pollInFlight = false
    }
    if (get().job?.reviewId !== job.reviewId) return
    pollErrors = 0

    if (st.status === 'done') {
      finishJob()
      if (st.share_id && st.url) {
        const shareId = st.share_id
        const shareUrl = st.url
        set((s) => ({ shareUrls: { ...s.shareUrls, [shareId]: shareUrl } }))
      }
      void get().loadShares()
      const url = st.url
      rawToast.success(i18n.t('review.done_toast'), {
        duration: 15_000,
        ...(url
          ? {
              action: {
                label: i18n.t('review.open'),
                onClick: () => openExternal(url),
              },
            }
          : {}),
      })
      return
    }
    if (st.status === 'failed') {
      finishJob()
      // Server-fault failures refund the daily quota; either way the job is
      // over. Surface the server's reason verbatim.
      toast.error(
        i18n.t('review.failed_toast', { error: st.error ?? 'unknown error' }),
        { duration: 15_000 },
      )
      return
    }
    set({
      job: {
        ...job,
        status: st.status === 'running' ? 'running' : 'queued',
        progress:
          typeof st.progress === 'number'
            ? Math.min(Math.max(st.progress, 0), 1)
            : job.progress,
      },
    })
    schedulePoll(POLL_MS)
  }

  return {
    shares: null,
    sharesLoading: false,
    sharesError: null,
    job: null,
    submitting: false,
    gameMap: {},
    shareUrls: {},

    loadShares: async () => {
      const api = reviewApiCfg()
      if (!api) return
      ensureHydrated()
      set({ sharesLoading: true, sharesError: null })
      try {
        const shares = await invoke<ShareEntry[]>('native_api_list_shares', {
          baseUrl: api.baseUrl,
          proxy: api.proxy,
          key: api.key,
        })
        set({ shares, sharesLoading: false })
      } catch (e) {
        // Keep any previously loaded list — the error renders as a banner,
        // not as a replacement for results the user already has.
        set({ sharesError: String(e), sharesLoading: false })
      }
    },

    resolveShareUrl: async (share) => {
      const cached = get().shareUrls[share.share_id]
      if (cached) return cached
      const api = reviewApiCfg()
      if (!api) return null
      ensureHydrated()
      try {
        // The job poll answers with the job's *current* share URL — for a
        // share that is still listed, that is this one.
        const st = await invoke<ReviewJobStatus>('native_api_review_status', {
          baseUrl: api.baseUrl,
          proxy: api.proxy,
          key: api.key,
          reviewId: share.review_id,
        })
        let url = st.share_id === share.share_id ? (st.url ?? null) : null
        if (!url) {
          // Revoked (or rotated) between listing and click: a share re-issue
          // returns the live link.
          const issued = await invoke<ShareIssued>('native_api_review_share', {
            baseUrl: api.baseUrl,
            proxy: api.proxy,
            key: api.key,
            reviewId: share.review_id,
          })
          url = issued.url
          void get().loadShares()
        }
        set((s) => ({
          shareUrls: { ...s.shareUrls, [share.share_id]: url },
        }))
        return url
      } catch (e) {
        toast.error(String(e))
        return null
      }
    },

    submit: async (record, modelOverride) => {
      const api = reviewApiCfg()
      if (!api) return false
      ensureHydrated()
      if (get().job || get().submitting) return false
      const cfg = useConfigStore.getState().config
      const model =
        modelOverride ??
        (record.num_players === 3
          ? (cfg?.bot.api.model_3p ?? '')
          : (cfg?.bot.api.model_4p ?? ''))
      set({ submitting: true })
      try {
        const resp = await invoke<ReviewSubmitted>(
          'native_api_review_history_game',
          {
            baseUrl: api.baseUrl,
            proxy: api.proxy,
            key: api.key,
            id: record.id,
            model,
          },
        )
        const job: ActiveJob = {
          reviewId: resp.review_id,
          historyId: record.id,
          status: 'queued',
          progress: 0,
        }
        const gameMap = { ...get().gameMap, [record.id]: resp.review_id }
        saveJson(gameMapKey(), gameMap)
        saveJson(pendingJobKey(), job)
        set({ job, gameMap, submitting: false })
        toast.info(i18n.t('review.queued_toast'))
        schedulePoll(POLL_MS)
        return true
      } catch (e) {
        set({ submitting: false })
        // The message already carries the server's reason and any
        // Retry-After hint (cooldown, daily quota, queue full).
        toast.error(String(e), { duration: 10_000 })
        // The submit is not idempotent: on an ambiguous failure (timeout
        // after the server accepted) the job exists server-side with no
        // local review_id. Refreshing the list is how such an orphan
        // resurfaces once it finishes.
        void get().loadShares()
        return false
      }
    },

    resume: () => {
      ensureHydrated()
      if (get().job && pollTimer === null && !pollInFlight) {
        schedulePoll(0)
      }
    },

    revoke: async (shareId) => {
      const api = reviewApiCfg()
      if (!api) return
      try {
        await invoke('native_api_revoke_share', {
          baseUrl: api.baseUrl,
          proxy: api.proxy,
          key: api.key,
          shareId,
        })
        set((s) => ({
          shares: s.shares?.filter((sh) => sh.share_id !== shareId) ?? null,
        }))
        toast.success(i18n.t('review.revoked_toast'))
      } catch (e) {
        toast.error(String(e))
      }
    },

    reshare: async (historyId) => {
      const api = reviewApiCfg()
      if (!api) return null
      ensureHydrated()
      const reviewId = get().gameMap[historyId]
      if (!reviewId) return null
      try {
        const issued = await invoke<ShareIssued>('native_api_review_share', {
          baseUrl: api.baseUrl,
          proxy: api.proxy,
          key: api.key,
          reviewId,
        })
        void get().loadShares()
        return issued.url
      } catch (e) {
        const msg = String(e)
        if (isHttp404(msg)) {
          // The review was evicted server-side (or belongs to another key):
          // drop the stale mapping so the game can be submitted afresh.
          const gameMap = { ...get().gameMap }
          delete gameMap[historyId]
          saveJson(gameMapKey(), gameMap)
          set({ gameMap })
          toast.error(i18n.t('review.review_gone'))
        } else {
          toast.error(msg)
        }
        return null
      }
    },
  }
})
