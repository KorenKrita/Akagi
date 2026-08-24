// Review status of a single history game, shared by the History list rows
// and the game-detail dialog. The pure function exists because GameList
// renders rows inline in a map() — a hook per row is not an option there —
// while the detail dialog (one record at a time) uses the hook.

import { useConfigStore } from '@/stores/configStore'
import { useReviewStore, type ActiveJob } from '@/stores/reviewStore'
import type { GameRecord, ShareEntry } from '@/types'

export type GameReviewStatus =
  /** No API key configured, or an observed game with no seat — show nothing. */
  | { kind: 'hidden' }
  /** Not reviewed yet — offer the submit flow. */
  | { kind: 'none' }
  /** The active background job is reviewing this game. */
  | { kind: 'reviewing' }
  /** Reviewed, live share link — offer open. */
  | { kind: 'reviewed'; share: ShareEntry }
  /** Reviewed locally but the share list hasn't loaded yet — show the state,
   *  disable the action until the list answers. */
  | { kind: 'reviewed_loading' }
  /** Reviewed but the link was revoked (or the review evicted) — offer a
   *  re-share, which self-heals the evicted case. */
  | { kind: 'revoked' }

export type ReviewStatusDeps = {
  configured: boolean
  job: ActiveJob | null
  gameMap: Record<string, string>
  shares: ShareEntry[] | null
}

export function computeGameReviewStatus(
  record: GameRecord,
  { configured, job, gameMap, shares }: ReviewStatusDeps,
): GameReviewStatus {
  if (!configured || record.our_seat == null) return { kind: 'hidden' }
  if (job?.historyId === record.id) return { kind: 'reviewing' }
  const reviewId = gameMap[record.id]
  if (!reviewId) return { kind: 'none' }
  if (shares === null) return { kind: 'reviewed_loading' }
  const share = shares.find((s) => s.review_id === reviewId)
  return share ? { kind: 'reviewed', share } : { kind: 'revoked' }
}

/** Subscribe to everything `computeGameReviewStatus` needs. Components that
 *  render many records subscribe once and call the pure function per row. */
export function useReviewStatusDeps(): ReviewStatusDeps {
  const config = useConfigStore((s) => s.config)
  const job = useReviewStore((s) => s.job)
  const gameMap = useReviewStore((s) => s.gameMap)
  const shares = useReviewStore((s) => s.shares)
  const api = config?.bot.api
  const configured =
    !!api && api.base_url.trim() !== '' && api.key.trim() !== ''
  return { configured, job, gameMap, shares }
}

export function useGameReviewStatus(record: GameRecord | null): GameReviewStatus {
  const deps = useReviewStatusDeps()
  if (!record) return { kind: 'hidden' }
  return computeGameReviewStatus(record, deps)
}
