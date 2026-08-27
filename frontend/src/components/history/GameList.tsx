// Filtered + sorted (newest-first) list of game records. Click any row
// to open the detail dialog. Delete button on each row asks for
// confirmation before invoking the backend command.
//
// Review integration (Beta): rather than a new column — the table is
// already at capacity — the review affordance shares the trailing actions
// cell with delete, as one status-driven icon: not reviewed → submit flow
// (the Review page's confirm dialog, pre-selected to this game), reviewed →
// open the result, link revoked → re-issue, reviewing → spinner. Rows the
// feature can't apply to (no API key, observer games) show no icon at all.

import { useEffect, useState } from 'react'
import { Loader2, SearchCheck, Share2, Trash2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

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
import {
  computeGameReviewStatus,
  useReviewStatusDeps,
} from '@/components/review/gameReviewStatus'
import { ReviewSubmitDialog } from '@/components/review/ReviewSubmitDialog'
import { useKeyStatus } from '@/hooks/useKeyStatus'
import { invoke } from '@/lib/tauri'
import { openExternal } from '@/lib/external'
import { roomLabelKey } from '@/lib/matchInfo'
import { computePt, type PtRule } from '@/lib/ptCalc'
import { useConfigStore } from '@/stores/configStore'
import { useHistoryStore } from '@/stores/historyStore'
import { useReviewStore } from '@/stores/reviewStore'
import type { GameRecord, ShareEntry } from '@/types'

import { GameDetailDialog } from './GameDetailDialog'

function modeLabel(record: GameRecord, t: (k: string) => string): string {
  const players = record.num_players === 3 ? '3p' : '4p'
  const mode =
    record.kyoku_mode === 'east_only'
      ? t('history.filter.east_only')
      : record.kyoku_mode === 'east_south'
        ? t('history.filter.east_south')
        : t('history.filter.other')
  return `${players} · ${mode}`
}

export function GameList({
  records,
  rule,
}: {
  records: GameRecord[]
  rule: PtRule
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState<GameRecord | null>(null)
  const [reviewRec, setReviewRec] = useState<GameRecord | null>(null)
  const [rowBusy, setRowBusy] = useState<string | null>(null)
  const remove = useHistoryStore((s) => s.remove)

  const reviewDeps = useReviewStatusDeps()
  const resolveShareUrl = useReviewStore((s) => s.resolveShareUrl)
  const reshare = useReviewStore((s) => s.reshare)

  const config = useConfigStore((s) => s.config)
  const api = config?.bot.api
  const proxy = api?.proxy_enabled ? api.proxy.trim() : ''
  const keyStatus = useKeyStatus(api?.base_url ?? '', proxy, api?.use_system_proxy ?? false, api?.key ?? '')

  // The status badges join the per-key gameMap against the live share list;
  // hydrate the store's namespace and fetch the list once when the History
  // page is the first review surface the user visits this session.
  const apiKey = api?.key ?? ''
  useEffect(() => {
    if (!reviewDeps.configured) return
    const store = useReviewStore.getState()
    store.resume()
    if (store.shares === null) void store.loadShares()
  }, [reviewDeps.configured, apiKey])

  const onDelete = async (id: string) => {
    if (!window.confirm(t('history.delete_confirm'))) return
    try {
      const removed = await invoke<boolean>('delete_game_history_entry', { id })
      if (removed) {
        remove(id)
        toast.success(t('history.deleted'))
      }
    } catch (e) {
      toast.error(String(e))
    }
  }

  const onOpenShare = async (rowId: string, share: ShareEntry) => {
    setRowBusy(rowId)
    const url = await resolveShareUrl(share)
    setRowBusy(null)
    if (url) openExternal(url)
  }

  const onReshare = async (rowId: string) => {
    setRowBusy(rowId)
    const url = await reshare(rowId)
    setRowBusy(null)
    if (url) openExternal(url)
  }

  const reviewSlot = (r: GameRecord) => {
    const status = computeGameReviewStatus(r, reviewDeps)
    switch (status.kind) {
      case 'hidden':
        return null
      case 'none':
        return (
          <Button
            variant="ghost"
            size="sm"
            aria-label={t('review.review_this_game')}
            title={t('review.review_this_game')}
            onClick={(e) => {
              e.stopPropagation()
              setReviewRec(r)
            }}
          >
            <SearchCheck className="h-4 w-4 text-muted-foreground" />
          </Button>
        )
      case 'reviewing':
        return (
          <Button
            variant="ghost"
            size="sm"
            disabled
            aria-label={t('review.job_title')}
            title={t('review.job_title')}
          >
            <Loader2 className="h-4 w-4 animate-spin" />
          </Button>
        )
      case 'reviewed':
      case 'reviewed_loading':
        return (
          <Button
            variant="ghost"
            size="sm"
            disabled={status.kind === 'reviewed_loading' || rowBusy === r.id}
            aria-label={t('review.open_review')}
            title={t('review.open_review')}
            onClick={(e) => {
              e.stopPropagation()
              if (status.kind === 'reviewed') void onOpenShare(r.id, status.share)
            }}
          >
            <SearchCheck className="h-4 w-4 text-emerald-500" />
          </Button>
        )
      case 'revoked':
        return (
          <Button
            variant="ghost"
            size="sm"
            disabled={rowBusy === r.id}
            aria-label={t('review.reshare')}
            title={t('review.reshare')}
            onClick={(e) => {
              e.stopPropagation()
              void onReshare(r.id)
            }}
          >
            <Share2 className="h-4 w-4 text-muted-foreground" />
          </Button>
        )
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm uppercase tracking-wider">
          {t('history.game_list')}
        </CardTitle>
      </CardHeader>
      <CardContent className="px-0">
        {records.length === 0 ? (
          <div className="text-sm text-muted-foreground py-8 text-center">
            {t('history.no_data')}
          </div>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t('history.table.date')}</TableHead>
                <TableHead>{t('history.table.platform')}</TableHead>
                <TableHead>{t('history.table.room')}</TableHead>
                <TableHead>{t('history.table.mode')}</TableHead>
                <TableHead>{t('history.table.rank')}</TableHead>
                <TableHead className="text-right">
                  {t('history.table.end_score')}
                </TableHead>
                <TableHead className="text-right">{t('history.table.pt')}</TableHead>
                <TableHead className="w-20" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {records.map((r) => {
                const pt = computePt(r, rule)
                const score =
                  r.our_seat == null ? null : r.final_scores[r.our_seat]
                const room = roomLabelKey(r.match_info)
                return (
                  <TableRow
                    key={r.id}
                    className="cursor-pointer"
                    onClick={() => setOpen(r)}
                  >
                    <TableCell className="font-mono text-xs">
                      {new Date(r.started_at).toLocaleString()}
                    </TableCell>
                    <TableCell>
                      {t(`platform.${r.platform}`)}
                    </TableCell>
                    <TableCell>
                      {room ? t(room.key, room.params) : '—'}
                    </TableCell>
                    <TableCell>{modeLabel(r, t)}</TableCell>
                    <TableCell>{r.our_rank ?? '—'}</TableCell>
                    <TableCell className="text-right font-mono">
                      {score == null ? '—' : score.toLocaleString()}
                    </TableCell>
                    <TableCell
                      className={
                        'text-right font-mono ' +
                        (pt > 0
                          ? 'text-emerald-500'
                          : pt < 0
                            ? 'text-red-500'
                            : '')
                      }
                    >
                      {r.our_rank == null ? '—' : pt.toFixed(1)}
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="inline-flex gap-0.5">
                        {reviewSlot(r)}
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={(e) => {
                            e.stopPropagation()
                            void onDelete(r.id)
                          }}
                          aria-label={t('history.table.delete')}
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
      </CardContent>
      <GameDetailDialog
        record={open}
        onOpenChange={(v) => !v && setOpen(null)}
        onReview={(r) => {
          // Hand off to the submit dialog — stacking two modals would bury
          // the confirm step behind the detail dialog's backdrop.
          setOpen(null)
          setReviewRec(r)
        }}
      />
      <ReviewSubmitDialog
        open={reviewRec !== null}
        onOpenChange={(v) => !v && setReviewRec(null)}
        keyStatus={keyStatus}
        initialRecord={reviewRec}
      />
    </Card>
  )
}
