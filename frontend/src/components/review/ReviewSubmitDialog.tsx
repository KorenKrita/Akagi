// Two-step submit dialog for the Review page (Beta): pick a recorded game,
// then confirm with a model choice. Living in a dialog keeps the (possibly
// very long) history list from burying the finished-reviews table on the
// page — the list scrolls inside the dialog instead.
//
// The picker is a real `Table` (auto-layout columns — the same component the
// History page uses), NOT a hand-rolled fixed-width grid: table layout keeps
// every column aligned at any width and can't overflow horizontally. The
// dialog is widened via `sm:max-w-3xl`, which (through cn's tailwind-merge)
// REPLACES DialogContent's built-in `sm:max-w-sm` — a plain `max-w-*` class
// would silently lose to it at desktop widths and crush the table.
//
// The model list comes from `GET /v3/models` (the key's plan decides what is
// visible) and is filtered to the picked game's player count. "Server
// default" submits an empty model id, which the backend resolves per the
// documented default rules (a 3p game falls back to the "3p" alias there).

import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ExternalLink, Loader2, SearchCheck, Share2 } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { invoke } from '@/lib/tauri'
import { openExternal } from '@/lib/external'
import { roomLabelKey } from '@/lib/matchInfo'
import { useConfigStore } from '@/stores/configStore'
import { useHistoryStore } from '@/stores/historyStore'
import { reviewApiCfg, useReviewStore } from '@/stores/reviewStore'
import type { GameRecord, KeyStatus, ModelInfo, ShareEntry } from '@/types'

/** Radix Select forbids an empty item value; this sentinel stands in for
 *  "no explicit model — let the server pick its default". */
const SERVER_DEFAULT = '__server_default__'

/** Sticky header cell inside the scrolling table wrapper. `bg-popover`
 *  matches the dialog surface so rows vanish under it cleanly. */
const STICKY_HEAD = 'sticky top-0 z-10 bg-popover'

export function ReviewSubmitDialog({
  open,
  onOpenChange,
  keyStatus,
  initialRecord,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  keyStatus: KeyStatus | null
  /** Skip the picker and land directly on the confirm step for this game —
   *  the History page's per-row "review" entry point. */
  initialRecord?: GameRecord | null
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        {/* All picker state lives in the body, which mounts fresh with the
            dialog content — closing and reopening resets to step 1 without
            any effect-driven state juggling. */}
        {open && (
          <DialogBody
            onOpenChange={onOpenChange}
            keyStatus={keyStatus}
            initialRecord={initialRecord ?? null}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function DialogBody({
  onOpenChange,
  keyStatus,
  initialRecord,
}: {
  onOpenChange: (open: boolean) => void
  keyStatus: KeyStatus | null
  initialRecord: GameRecord | null
}) {
  const { t } = useTranslation()
  const records = useHistoryStore((s) => s.records)
  const job = useReviewStore((s) => s.job)
  const submitting = useReviewStore((s) => s.submitting)
  const gameMap = useReviewStore((s) => s.gameMap)
  const shares = useReviewStore((s) => s.shares)
  const submit = useReviewStore((s) => s.submit)
  const reshare = useReviewStore((s) => s.reshare)
  const resolveShareUrl = useReviewStore((s) => s.resolveShareUrl)
  const config = useConfigStore((s) => s.config)

  const [mode, setMode] = useState<'4' | '3'>(
    initialRecord?.num_players === 3 ? '3' : '4',
  )
  const [selected, setSelected] = useState<GameRecord | null>(initialRecord)
  const [model, setModel] = useState<string>(SERVER_DEFAULT)
  // null = loading, [] = loaded empty, 'error' = fetch failed (default only).
  const [models, setModels] = useState<ModelInfo[] | 'error' | null>(null)
  const [busy, setBusy] = useState(false)

  // The model-list fetch below needs the *current* selection when it lands
  // (an initialRecord is selected before the list arrives) — a ref sidesteps
  // the stale closure without re-running the fetch on every selection.
  const selectedRef = useRef<GameRecord | null>(initialRecord)
  const selectRecord = (r: GameRecord | null) => {
    selectedRef.current = r
    setSelected(r)
  }

  // Fetch the plan's model list once per dialog opening (it changes only
  // when the key/plan does).
  useEffect(() => {
    const api = reviewApiCfg()
    if (!api) return
    let cancelled = false
    void invoke<ModelInfo[]>('native_api_models', {
      baseUrl: api.baseUrl,
      proxy: api.proxy,
      key: api.key,
    })
      .then((list) => {
        if (cancelled) return
        setModels(list)
        // A record picked before the list arrived (initialRecord) couldn't
        // preselect its configured model — do it now, unless the user
        // already chose one explicitly.
        const rec = selectedRef.current
        if (rec) {
          const cfg = useConfigStore.getState().config
          const configured = (
            rec.num_players === 3
              ? (cfg?.bot.api.model_3p ?? '')
              : (cfg?.bot.api.model_4p ?? '')
          ).trim()
          if (configured && list.some((m) => m.id === configured)) {
            setModel((prev) => (prev === SERVER_DEFAULT ? configured : prev))
          }
        }
      })
      .catch(() => {
        if (!cancelled) setModels('error')
      })
    return () => {
      cancelled = true
    }
  }, [])

  const shareByReview = useMemo(() => {
    const m = new Map<string, ShareEntry>()
    for (const s of shares ?? []) m.set(s.review_id, s)
    return m
  }, [shares])

  const filtered = useMemo(
    () => records.filter((r) => r.num_players === (mode === '3' ? 3 : 4)),
    [records, mode],
  )

  const gameModels = useMemo(() => {
    if (!Array.isArray(models) || !selected) return []
    const game = selected.num_players === 3 ? '3p' : '4p'
    return models.filter((m) => m.game === game)
  }, [models, selected])

  const pick = (r: GameRecord) => {
    selectRecord(r)
    // Preselect the configured model for this game's mode when the plan
    // actually offers it; otherwise the server default.
    const configured =
      r.num_players === 3
        ? (config?.bot.api.model_3p ?? '')
        : (config?.bot.api.model_4p ?? '')
    const available =
      Array.isArray(models) && models.some((m) => m.id === configured.trim())
    setModel(available ? configured.trim() : SERVER_DEFAULT)
  }

  const planBlocked = keyStatus !== null && keyStatus.reviews_per_day <= 0
  const startBlocked = job !== null || submitting || planBlocked

  const onStart = async () => {
    if (!selected) return
    const ok = await submit(selected, model === SERVER_DEFAULT ? '' : model)
    if (ok) onOpenChange(false)
  }

  const reviewId = selected ? gameMap[selected.id] : undefined
  const existingShare = reviewId ? shareByReview.get(reviewId) : undefined
  const linkRevoked = !!reviewId && !existingShare && shares !== null

  const onOpenExisting = async () => {
    if (!existingShare) return
    setBusy(true)
    const url = await resolveShareUrl(existingShare)
    setBusy(false)
    if (url) openExternal(url)
  }

  const onReshare = async () => {
    if (!selected) return
    setBusy(true)
    const url = await reshare(selected.id)
    setBusy(false)
    if (url) openExternal(url)
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>{t('review.games_title')}</DialogTitle>
        <DialogDescription>
          {selected ? t('review.confirm_hint') : t('review.pick_game')}
        </DialogDescription>
      </DialogHeader>

      {selected === null ? (
        <div className="space-y-3">
          <Tabs value={mode} onValueChange={(v) => setMode(v === '3' ? '3' : '4')}>
            <TabsList>
              <TabsTrigger value="4">
                {t('history.filter.num_players_4')}
              </TabsTrigger>
              <TabsTrigger value="3">
                {t('history.filter.num_players_3')}
              </TabsTrigger>
            </TabsList>
          </Tabs>
          {filtered.length === 0 ? (
            <div className="text-sm text-muted-foreground py-8 text-center">
              {t('history.no_data')}
            </div>
          ) : (
            <div className="max-h-[55vh] overflow-y-auto rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className={STICKY_HEAD}>
                      {t('history.table.date')}
                    </TableHead>
                    <TableHead className={STICKY_HEAD}>
                      {t('history.table.platform')}
                    </TableHead>
                    <TableHead className={STICKY_HEAD}>
                      {t('history.table.room')}
                    </TableHead>
                    <TableHead className={STICKY_HEAD}>
                      {t('history.table.mode')}
                    </TableHead>
                    <TableHead className={`${STICKY_HEAD} text-right`}>
                      {t('history.table.rank')}
                    </TableHead>
                    <TableHead className={`${STICKY_HEAD} text-right`} />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {filtered.map((r) => {
                    const rid = gameMap[r.id]
                    const hasShare = rid ? shareByReview.has(rid) : false
                    const revoked = !!rid && !hasShare && shares !== null
                    const noSeat = r.our_seat == null
                    const room = roomLabelKey(r.match_info)
                    return (
                      <TableRow
                        key={r.id}
                        onClick={noSeat ? undefined : () => pick(r)}
                        title={noSeat ? t('review.no_seat_hint') : undefined}
                        className={
                          noSeat ? 'opacity-50' : 'cursor-pointer'
                        }
                      >
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {new Date(r.started_at).toLocaleString()}
                        </TableCell>
                        <TableCell className="whitespace-nowrap">
                          {t(`platform.${r.platform}`)}
                        </TableCell>
                        <TableCell className="whitespace-nowrap">
                          {room ? t(room.key, room.params) : '—'}
                        </TableCell>
                        <TableCell className="whitespace-nowrap text-muted-foreground">
                          {kyokuLabel(r, t)}
                        </TableCell>
                        <TableCell className="text-right font-mono">
                          {r.our_rank ?? '—'}
                        </TableCell>
                        <TableCell className="text-right">
                          {hasShare ? (
                            <Badge variant="secondary">
                              {t('review.status_reviewed')}
                            </Badge>
                          ) : revoked ? (
                            <Badge variant="outline">
                              {t('review.status_link_revoked')}
                            </Badge>
                          ) : null}
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          )}
        </div>
      ) : (
        <div className="space-y-4">
          <GameSummary record={selected} />

          <div className="space-y-1">
            <p className="text-sm font-medium">{t('review.model_label')}</p>
            {models === null ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                {t('common.loading')}
              </div>
            ) : (
              <>
                <Select value={model} onValueChange={setModel}>
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={SERVER_DEFAULT}>
                      {t('review.model_server_default')}
                    </SelectItem>
                    {gameModels.map((m) => (
                      <SelectItem key={m.id} value={m.id}>
                        <span className="font-mono">{m.id}</span>
                        {m.desc && (
                          <span className="text-muted-foreground">
                            {' '}
                            — {m.desc}
                          </span>
                        )}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {models === 'error' && (
                  <p className="text-xs text-muted-foreground">
                    {t('review.model_load_failed')}
                  </p>
                )}
              </>
            )}
          </div>

          {reviewId && (
            <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 space-y-2">
              <p className="text-xs">{t('review.already_reviewed_hint')}</p>
              <div className="flex gap-2">
                {existingShare && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => void onOpenExisting()}
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                    {t('review.open_existing')}
                  </Button>
                )}
                {linkRevoked && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => void onReshare()}
                  >
                    <Share2 className="h-3.5 w-3.5" />
                    {t('review.reshare')}
                  </Button>
                )}
              </div>
            </div>
          )}

          <div className="text-xs text-muted-foreground space-y-1">
            {keyStatus !== null && keyStatus.reviews_per_day > 0 && (
              <p>
                {t('review.quota', {
                  used: keyStatus.reviews_today,
                  limit: keyStatus.reviews_per_day,
                })}
              </p>
            )}
            <p>{t('review.cooldown_hint')}</p>
          </div>

          <div className="flex justify-between">
            <Button variant="ghost" onClick={() => selectRecord(null)}>
              {t('common.back')}
            </Button>
            <Button disabled={startBlocked} onClick={() => void onStart()}>
              {submitting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <SearchCheck className="h-3.5 w-3.5" />
              )}
              {planBlocked
                ? t('review.plan_no_reviews')
                : job !== null
                  ? t('review.job_title')
                  : t('review.start_review')}
            </Button>
          </div>
        </div>
      )}
    </>
  )
}

/** The picked game, spelled out — the user is about to spend a daily review
 *  on it, so leave no doubt about which game it is. */
function GameSummary({ record }: { record: GameRecord }) {
  const { t } = useTranslation()
  const room = roomLabelKey(record.match_info)
  const rows: Array<[string, string]> = [
    [t('history.table.date'), new Date(record.started_at).toLocaleString()],
    [t('history.table.platform'), t(`platform.${record.platform}`)],
    [t('history.table.room'), room ? t(room.key, room.params) : '—'],
    [
      t('history.table.mode'),
      `${record.num_players === 3 ? '3p' : '4p'} · ${kyokuLabel(record, t)}`,
    ],
    [t('history.table.rank'), record.our_rank?.toString() ?? '—'],
  ]
  return (
    <div className="rounded-md border px-4 py-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-1 text-sm">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <span className="text-muted-foreground">{label}</span>
          <span>{value}</span>
        </div>
      ))}
    </div>
  )
}

function kyokuLabel(r: GameRecord, t: (k: string) => string): string {
  return r.kyoku_mode === 'east_only'
    ? t('history.filter.east_only')
    : r.kyoku_mode === 'east_south'
      ? t('history.filter.east_south')
      : t('history.filter.other')
}
