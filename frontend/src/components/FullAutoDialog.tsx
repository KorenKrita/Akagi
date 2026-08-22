import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { PlayCircle } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/sonner'
import { useConfigStore } from '@/stores/configStore'
import type { AutoplayConfig } from '@/types'

type AutoplaySessionStatus = {
  active: boolean
  target_games: number | null
  games_completed: number
  stop_reason: string | null
  queue_seconds: number | null
  between_games_seconds: number | null
}

function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60)
  const s = seconds % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

/** Game-tab entry point for full auto: a header button opening the session
 *  options in a modal. Starting saves the options and launches the session;
 *  the button doubles as the live status line while it runs. */
export function FullAutoDialog() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const setConfig = useConfigStore((s) => s.setConfig)
  const [open, setOpen] = useState(false)
  const [status, setStatus] = useState<AutoplaySessionStatus | null>(null)
  const [games, setGames] = useState('')
  const [busy, setBusy] = useState(false)
  const [availableRooms, setAvailableRooms] = useState<string[] | null>(null)

  const rc = config?.autoplay.riichi_city
  const [draft, setDraft] = useState<AutoplayConfig['riichi_city'] | null>(null)

  // Fetch the rooms the player can queue in when the dialog opens, and
  // again after each completed game (a rank-up can unlock a higher room).
  // The server's classify list already gates by rank; null = all rooms
  // shown (not connected or fetch failed).
  useEffect(() => {
    if (!open) return
    let alive = true
    invoke<{ rooms: string[] }>('riichi_city_available_rooms')
      .then((result) => {
        const rooms = result.rooms
        if (alive && rooms.length > 0) {
          setAvailableRooms(rooms)
          // If the configured room isn't offered, auto-select the
          // highest available.
          setDraft((d) => {
            const base = d ?? rc
            if (!base) return d
            if (rooms.includes(base.room)) return d
            const highest = ['galaxy', 'sun', 'moon', 'star'].find((r) =>
              rooms.includes(r),
            )
            return highest
              ? { ...base, room: highest as AutoplayConfig['riichi_city']['room'] }
              : d
          })
        }
      })
      .catch(() => {
        // Not connected or fetch failed — show all rooms.
        if (alive) setAvailableRooms(null)
      })
    return () => {
      alive = false
    }
  }, [open, rc, status?.games_completed])

  // The dialog body renders only when open; the draft is nil until then,
  // so the config seeds it on first render rather than in an effect.
  const activeDraft = draft ?? (open && rc ? rc : null)
  const patch = (p: Partial<AutoplayConfig['riichi_city']>) =>
    setDraft((d) => ({ ...(d ?? (open && rc ? rc : undefined)!), ...p }))

  // Session status polling (also while closed, so the header stays honest).
  // 1s while a session runs so the queue timer ticks every second; 3s idle.
  useEffect(() => {
    let alive = true
    const poll = async () => {
      try {
        const s = await invoke<AutoplaySessionStatus>('autoplay_session_status')
        if (alive) setStatus(s)
      } catch {
        /* backend absent (no Tauri) — leave the status inert */
      }
    }
    poll()
    const timer = window.setInterval(poll, status?.active ? 1000 : 3000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [status?.active])

  const start = async () => {
    if (!config || !activeDraft) return
    setBusy(true)
    try {
      const newConfig = {
        ...config,
        autoplay: { ...config.autoplay, riichi_city: activeDraft },
      }
      await invoke('update_config', { newConfig })
      setConfig(newConfig)
      const parsed = games.trim() === '' ? null : Math.max(1, Number(games))
      setStatus(
        await invoke<AutoplaySessionStatus>('autoplay_session_start', {
          games: parsed,
        }),
      )
      toast.success(t('settings.autoplay.session_started'))
      setOpen(false)
    } catch (e) {
      toast.error(String(e))
    } finally {
      setBusy(false)
    }
  }

  const stop = async () => {
    setBusy(true)
    try {
      setStatus(await invoke<AutoplaySessionStatus>('autoplay_session_stop'))
    } catch (e) {
      toast.error(String(e))
    } finally {
      setBusy(false)
    }
  }

  const progress = status
    ? `${status.games_completed}${status.target_games ? ` / ${status.target_games}` : ''}`
    : ''

  const statusSuffix = (seconds: number | null | undefined, key: string) =>
    status?.active && seconds != null
      ? ` · ${t(key, { time: formatDuration(seconds) })}`
      : ''

  const queueSuffix = statusSuffix(status?.queue_seconds, 'game.fullauto_queued')
  const waitingSuffix = statusSuffix(
    status?.between_games_seconds,
    'game.fullauto_waiting',
  )

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="text-xs">
          <PlayCircle className="size-4" />
          {status?.active
            ? t('game.fullauto_running', { count: progress }) +
              queueSuffix +
              waitingSuffix
            : t('game.fullauto_button')}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('game.fullauto_title')}</DialogTitle>
          <DialogDescription>
            {t('settings.autoplay.session_idle')}
          </DialogDescription>
        </DialogHeader>

        {activeDraft && (
          <div className="grid gap-4 py-2">
            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.queue_room')}</Label>
                <Select
                  value={activeDraft.room}
                  onValueChange={(v) => {
                    const room = v as AutoplayConfig['riichi_city']['room']
                    patch(
                      room === 'galaxy'
                        ? { room }
                        : { room, galaxy_fallback_sun: false },
                    )
                  }}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  {/* popper: item-aligned content detaches from its
                      trigger inside a portaled dialog. */}
                  <SelectContent position="popper">
                    {(['star', 'moon', 'sun', 'galaxy'] as const)
                      .filter((r) => !availableRooms || availableRooms.includes(r))
                      .map((r) => (
                        <SelectItem key={r} value={r}>
                          {r.charAt(0).toUpperCase() + r.slice(1)}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
                {availableRooms &&
                  !availableRooms.includes(activeDraft.room) && (
                    <p className="text-xs text-muted-foreground">
                      {t('game.fullauto_room_auto_switched')}
                    </p>
                  )}
              </div>
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.queue_game_type')}</Label>
                <Select
                  value={activeDraft.game_type}
                  onValueChange={(v) =>
                    patch({
                      game_type: v as AutoplayConfig['riichi_city']['game_type'],
                    })
                  }
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent position="popper">
                    <SelectItem value="east_only">
                      {t('settings.autoplay.queue_game_type_east_only')}
                    </SelectItem>
                    <SelectItem value="hanchan">
                      {t('settings.autoplay.queue_game_type_hanchan')}
                    </SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>

            {/* The sun-fallback option only exists in the galaxy room,
                inline with the label rather than pushed to the far right. */}
            {activeDraft.room === 'galaxy' && (
              <div className="flex items-center gap-2">
                <Switch
                  checked={activeDraft.galaxy_fallback_sun}
                  onCheckedChange={(v) => patch({ galaxy_fallback_sun: v })}
                />
                <Label className="text-xs font-normal">
                  {t('settings.autoplay.queue_fallback_sun')}
                </Label>
              </div>
            )}

            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.inter_game_delay')}</Label>
                <Input
                  type="number"
                  inputMode="numeric"
                  min={0}
                  value={activeDraft.inter_game_delay_ms}
                  onChange={(e) =>
                    patch({
                      inter_game_delay_ms: Math.max(0, Number(e.target.value || 0)),
                    })
                  }
                />
              </div>
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.session_games_placeholder')}</Label>
                <Input
                  type="number"
                  inputMode="numeric"
                  min={1}
                  placeholder="∞"
                  value={games}
                  disabled={status?.active ?? false}
                  onChange={(e) => setGames(e.target.value)}
                />
              </div>
            </div>

            {status?.active ? (
              <div className="grid gap-2">
                <p className="text-xs text-muted-foreground">
                  {t('settings.autoplay.session_running', { count: progress })}
                  {queueSuffix}
                  {waitingSuffix}
                </p>
                <Button variant="destructive" onClick={stop} disabled={busy}>
                  {t('settings.autoplay.session_stop')}
                </Button>
              </div>
            ) : (
              <Button onClick={start} disabled={busy}>
                {t('settings.autoplay.session_start')}
              </Button>
            )}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
