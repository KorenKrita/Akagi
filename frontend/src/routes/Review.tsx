// Whole-game review page (Beta). Submit a recorded history game to the
// inference server's background review queue and read the finished result
// in the MJOT viewer — there is no in-app result renderer yet, so every
// "open" hands the share URL to the system browser.
//
// Layout: the finished-reviews table IS the page (plus the active-job card
// while one runs). Picking a game to submit lives in `ReviewSubmitDialog` —
// a history of hundreds of games would otherwise bury the results table.
//
// The page is a thin view over `useReviewStore` (job polling, share cache,
// history↔review mapping live there and survive navigation). Everything is
// gated on a configured API key: `bot.api.base_url` + `bot.api.key`,
// deliberately NOT on `bot.api.enabled` (that switch routes live decisions;
// reviewing past games is useful either way).

import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import {
  Copy,
  ExternalLink,
  KeyRound,
  Loader2,
  SearchCheck,
  Trash2,
} from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { toast } from '@/components/ui/sonner'
import { ReviewSubmitDialog } from '@/components/review/ReviewSubmitDialog'
import { useKeyStatus } from '@/hooks/useKeyStatus'
import { copyText } from '@/lib/clipboard'
import { openExternal } from '@/lib/external'
import { useConfigStore } from '@/stores/configStore'
import { useHistoryStore } from '@/stores/historyStore'
import { useReviewStore } from '@/stores/reviewStore'
import type { GameRecord, KeyStatus, ShareEntry } from '@/types'

export function Review() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const api = config?.bot.api
  const configured =
    !!api && api.base_url.trim() !== '' && api.key.trim() !== ''
  const proxy = api?.proxy_enabled ? api.proxy.trim() : ''
  const keyStatus = useKeyStatus(api?.base_url ?? '', proxy, api?.use_system_proxy ?? false, api?.key ?? '')

  const job = useReviewStore((s) => s.job)
  const loadShares = useReviewStore((s) => s.loadShares)
  const resume = useReviewStore((s) => s.resume)
  const [pickerOpen, setPickerOpen] = useState(false)

  // Re-runs on a key change too: jobs, shares and the persisted mappings are
  // all per-key, and `resume()` re-hydrates the store's namespace for it.
  const apiKey = api?.key ?? ''
  useEffect(() => {
    if (!configured) return
    resume()
    void loadShares()
  }, [configured, apiKey, resume, loadShares])

  return (
    <div className="p-6 flex flex-col gap-6 w-full">
      <header className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-semibold flex items-center gap-2">
            {t('review.title')}
            <Badge
              variant="outline"
              className="border-amber-500/60 text-amber-500"
              title={t('review.beta_hint')}
            >
              {t('review.beta')}
            </Badge>
          </h1>
          <p className="text-sm text-muted-foreground">
            {t('review.description')}
          </p>
        </div>
        {configured && (
          <div className="flex items-center gap-4">
            <QuotaBadge status={keyStatus} />
            <Button onClick={() => setPickerOpen(true)}>
              <SearchCheck className="h-4 w-4" />
              {t('review.new_review')}
            </Button>
          </div>
        )}
      </header>

      {!configured ? (
        <NeedKeyCard />
      ) : (
        <>
          {job && <JobCard />}
          <SharesCard onNewReview={() => setPickerOpen(true)} />
          <ReviewSubmitDialog
            open={pickerOpen}
            onOpenChange={setPickerOpen}
            keyStatus={keyStatus}
          />
        </>
      )}
    </div>
  )
}

/** "Configure a key first" empty state — review is a keyed feature. */
function NeedKeyCard() {
  const { t } = useTranslation()
  return (
    <Card>
      <CardContent className="py-10 flex flex-col items-center gap-4 text-center">
        <KeyRound className="h-8 w-8 text-muted-foreground" />
        <div className="space-y-1">
          <p className="font-medium">{t('review.need_key_title')}</p>
          <p className="text-sm text-muted-foreground max-w-md">
            {t('review.need_key_body')}
          </p>
        </div>
        <Button asChild>
          <Link to="/bots">{t('review.go_configure')}</Link>
        </Button>
      </CardContent>
    </Card>
  )
}

/** Live review quota from `GET /v3/key`, when the key answers. */
function QuotaBadge({ status }: { status: KeyStatus | null }) {
  const { t } = useTranslation()
  if (!status) return null
  if (status.reviews_per_day <= 0) {
    return (
      <Badge variant="destructive">{t('review.plan_no_reviews')}</Badge>
    )
  }
  return (
    <div className="text-sm text-muted-foreground">
      {t('review.quota', {
        used: status.reviews_today,
        limit: status.reviews_per_day,
      })}
    </div>
  )
}

/** The queued/running job: status, progress bar, background hint. */
function JobCard() {
  const { t } = useTranslation()
  const job = useReviewStore((s) => s.job)
  const records = useHistoryStore((s) => s.records)
  if (!job) return null
  const record = records.find((r) => r.id === job.historyId)
  const pct = Math.round(job.progress * 100)
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm uppercase tracking-wider flex items-center gap-2">
          <Loader2 className="h-4 w-4 animate-spin" />
          {t('review.job_title')}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-center justify-between text-sm">
          <span>
            {record ? gameLabel(record) : job.reviewId}
            {' — '}
            {job.status === 'running'
              ? t('review.job_running')
              : t('review.job_queued')}
          </span>
          <span className="font-mono text-muted-foreground">{pct}%</span>
        </div>
        <div className="h-2 w-full rounded-full bg-muted overflow-hidden">
          <div
            className="h-full rounded-full bg-primary transition-[width] duration-500"
            style={{ width: `${pct}%` }}
          />
        </div>
        <p className="text-xs text-muted-foreground">
          {t('review.job_background_hint')}
        </p>
      </CardContent>
    </Card>
  )
}

/** Compact one-line label for a game record. */
function gameLabel(r: GameRecord): string {
  const date = new Date(r.started_at).toLocaleString()
  return `${date} · ${r.num_players}p`
}

/** Every live share link on this key — open, copy, or revoke. Includes
 *  reviews submitted from other installs of the same key. */
function SharesCard({ onNewReview }: { onNewReview: () => void }) {
  const { t } = useTranslation()
  const shares = useReviewStore((s) => s.shares)
  const sharesLoading = useReviewStore((s) => s.sharesLoading)
  const sharesError = useReviewStore((s) => s.sharesError)
  const loadShares = useReviewStore((s) => s.loadShares)
  const revoke = useReviewStore((s) => s.revoke)
  const resolveShareUrl = useReviewStore((s) => s.resolveShareUrl)
  const gameMap = useReviewStore((s) => s.gameMap)
  const records = useHistoryStore((s) => s.records)
  const [busy, setBusy] = useState<string | null>(null)

  const recordByReview = useMemo(() => {
    const m = new Map<string, GameRecord>()
    for (const [historyId, reviewId] of Object.entries(gameMap)) {
      const rec = records.find((r) => r.id === historyId)
      if (rec) m.set(reviewId, rec)
    }
    return m
  }, [gameMap, records])

  const onOpen = async (share: ShareEntry) => {
    setBusy(share.share_id)
    const url = await resolveShareUrl(share)
    setBusy(null)
    if (url) openExternal(url)
  }

  const onCopy = async (share: ShareEntry) => {
    setBusy(share.share_id)
    const url = await resolveShareUrl(share)
    const ok = url !== null && (await copyText(url))
    setBusy(null)
    if (ok) toast.success(t('review.copy_ok'))
    else if (url !== null) toast.error(t('review.copy_fail'))
  }

  const onRevoke = async (share: ShareEntry) => {
    if (!window.confirm(t('review.revoke_confirm'))) return
    setBusy(share.share_id)
    await revoke(share.share_id)
    setBusy(null)
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm uppercase tracking-wider flex items-center justify-between">
          {t('review.shares_title')}
          <Button
            variant="ghost"
            size="sm"
            disabled={sharesLoading}
            onClick={() => void loadShares()}
          >
            {t('common.refresh')}
          </Button>
        </CardTitle>
      </CardHeader>
      <CardContent className="px-0">
        {/* A failed refresh must not hide results the user already has:
            the error is a banner, the last-known list stays rendered. */}
        {sharesError && (
          <div className="text-sm text-red-500 px-6 pb-3">{sharesError}</div>
        )}
        {shares === null ? (
          !sharesError && (
            <div className="text-sm text-muted-foreground py-6 text-center">
              {t('common.loading')}
            </div>
          )
        ) : shares.length === 0 ? (
          <div className="py-8 flex flex-col items-center gap-3">
            <p className="text-sm text-muted-foreground">
              {t('review.shares_empty')}
            </p>
            <Button variant="outline" onClick={onNewReview}>
              <SearchCheck className="h-4 w-4" />
              {t('review.new_review')}
            </Button>
          </div>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t('review.table.date')}</TableHead>
                <TableHead>{t('review.table.game')}</TableHead>
                <TableHead>{t('review.table.model')}</TableHead>
                <TableHead className="text-right">
                  {t('review.table.decisions')}
                </TableHead>
                <TableHead className="text-right">
                  {t('review.table.match')}
                </TableHead>
                <TableHead className="text-right">
                  {t('review.table.actions')}
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shares.map((s) => {
                const rec = recordByReview.get(s.review_id)
                return (
                  <TableRow key={s.share_id}>
                    <TableCell className="font-mono text-xs">
                      {new Date(s.created_at).toLocaleString()}
                    </TableCell>
                    <TableCell className="text-sm">
                      {rec
                        ? `${t(`platform.${rec.platform}`)} · ${gameLabel(rec)}`
                        : t('review.table.unknown_game')}
                    </TableCell>
                    <TableCell className="font-mono text-xs">
                      {s.model ?? '—'}
                    </TableCell>
                    <TableCell className="text-right font-mono">
                      {s.summary?.n_decisions ?? '—'}
                    </TableCell>
                    <TableCell className="text-right font-mono">
                      {s.summary
                        ? `${(s.summary.match_rate * 100).toFixed(1)}%`
                        : '—'}
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="inline-flex gap-1">
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy === s.share_id}
                          onClick={() => void onOpen(s)}
                          aria-label={t('review.open')}
                          title={t('review.open')}
                        >
                          <ExternalLink className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy === s.share_id}
                          onClick={() => void onCopy(s)}
                          aria-label={t('review.copy')}
                          title={t('review.copy')}
                        >
                          <Copy className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy === s.share_id}
                          onClick={() => void onRevoke(s)}
                          aria-label={t('review.revoke')}
                          title={t('review.revoke')}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        )}
        <p className="text-xs text-muted-foreground px-6 pt-3">
          {t('review.anonymized_hint')}
        </p>
      </CardContent>
    </Card>
  )
}
