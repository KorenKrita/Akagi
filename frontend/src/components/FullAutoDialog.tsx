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

  const rc = config?.autoplay.riichi_city
  const [draft, setDraft] = useState<AutoplayConfig['riichi_city'] | null>(null)

  // Seed the draft from the live config whenever the dialog opens.
  useEffect(() => {
    if (open && rc) setDraft({ ...rc })
  }, [open, rc])

  // Session status polling (also while closed, so the header stays honest).
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
    const timer = window.setInterval(poll, 3000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [])

  const patch = (p: Partial<AutoplayConfig['riichi_city']>) =>
    setDraft((d) => (d ? { ...d, ...p } : d))

  const start = async () => {
    if (!config || !draft) return
    setBusy(true)
    try {
      const newConfig = {
        ...config,
        autoplay: { ...config.autoplay, riichi_city: draft },
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

  const queueSuffix =
    status?.active && status.queue_seconds != null
      ? ` · ${t('game.fullauto_queued', {
          time: formatDuration(status.queue_seconds),
        })}`
      : ''

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="text-xs">
          <PlayCircle className="size-4" />
          {status?.active
            ? t('game.fullauto_running', { count: progress }) + queueSuffix
            : t('game.fullauto_button')}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{t('game.fullauto_title')}</DialogTitle>
          <DialogDescription>
            {t('settings.autoplay.session_idle')}
          </DialogDescription>
        </DialogHeader>

        {draft && (
          <div className="grid gap-4 py-2">
            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.queue_room')}</Label>
                <Select
                  value={draft.room}
                  onValueChange={(v) =>
                    patch({ room: v as AutoplayConfig['riichi_city']['room'] })
                  }
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="star">Star</SelectItem>
                    <SelectItem value="moon">Moon</SelectItem>
                    <SelectItem value="sun">Sun</SelectItem>
                    <SelectItem value="galaxy">Galaxy</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.queue_game_type')}</Label>
                <Select
                  value={draft.game_type}
                  onValueChange={(v) =>
                    patch({
                      game_type: v as AutoplayConfig['riichi_city']['game_type'],
                    })
                  }
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
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

            {/* The sun-fallback option only exists in the galaxy room. */}
            {draft.room === 'galaxy' && (
              <div className="flex items-center justify-between gap-3">
                <Label className="text-xs font-normal">
                  {t('settings.autoplay.queue_fallback_sun')}
                </Label>
                <Switch
                  checked={draft.galaxy_fallback_sun}
                  onCheckedChange={(v) => patch({ galaxy_fallback_sun: v })}
                />
              </div>
            )}

            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label>{t('settings.autoplay.inter_game_delay')}</Label>
                <Input
                  type="number"
                  inputMode="numeric"
                  min={0}
                  value={draft.inter_game_delay_ms}
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
