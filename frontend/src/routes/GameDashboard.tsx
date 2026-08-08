import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import {
  Responsive,
  useContainerWidth,
  verticalCompactor,
  type Layout,
  type LayoutItem,
  type ResponsiveLayouts,
} from 'react-grid-layout'
import 'react-grid-layout/css/styles.css'
import 'react-resizable/css/styles.css'

import { Clock3, Gamepad2, HelpCircle } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/sonner'
import { useLayoutStore, visibleTilesFor } from '@/stores/layoutStore'
import { useNumPlayers } from '@/stores/gameStore'
import { useConfigStore } from '@/stores/configStore'
import { useUiPrefsStore } from '@/stores/uiPrefsStore'
import {
  BREAKPOINTS,
  COLS,
  type Breakpoint,
  type TileId,
} from '@/tiles/defaults'
import { renderTile } from '@/tiles/registry'
import { AddTileMenu } from '@/components/AddTileMenu'
import { DashboardOnboardingDialog } from '@/components/DashboardOnboardingDialog'
import { OverlayToggle } from '@/components/OverlayToggle'
import { Card, CardContent } from '@/components/ui/card'
import type { AutoJoinStatus } from '@/types'

const BOT_DISABLED_TOAST_ID = 'bot-disabled-warning'

export function GameDashboard() {
  const { t } = useTranslation()
  const layouts = useLayoutStore((s) => s.layouts)
  const hidden = useLayoutStore((s) => s.hidden)
  const mode = useLayoutStore((s) => s.mode)
  const setLayouts = useLayoutStore((s) => s.setLayouts)
  const setMode = useLayoutStore((s) => s.setMode)
  const reset = useLayoutStore((s) => s.reset)
  const numPlayers = useNumPlayers()
  const botEnabled = useConfigStore((s) => s.config?.bot.enabled)
  const markOnboarded = useUiPrefsStore((s) => s.markDashboardOnboarded)
  const { width, containerRef, mounted } = useContainerWidth()
  // Auto-open once on the very first visit. Derived from the persisted flag at
  // mount (read synchronously from localStorage at store init), so no effect is
  // needed and it never reappears after being dismissed.
  const [helpOpen, setHelpOpen] = useState(
    () => !useUiPrefsStore.getState().dashboardOnboarded,
  )

  // Sync layout mode with active game's player count.
  useEffect(() => {
    setMode(numPlayers === 3 ? '3p' : '4p')
  }, [numPlayers, setMode])

  // Warn when viewing Game page while bots are disabled.
  // botEnabled is undefined while config is still loading — wait for a real value.
  useEffect(() => {
    if (botEnabled === false) {
      toast.warning(t('game.bot_disabled_title'), {
        id: BOT_DISABLED_TOAST_ID,
        description: t('game.bot_disabled_desc'),
        duration: Infinity,
      })
    } else if (botEnabled === true) {
      toast.dismiss(BOT_DISABLED_TOAST_ID)
    }
    return () => {
      toast.dismiss(BOT_DISABLED_TOAST_ID)
    }
  }, [botEnabled, t])

  const handleHelpOpenChange = (open: boolean) => {
    setHelpOpen(open)
    if (!open) markOnboarded()
  }

  const [bp, setBp] = useState<Breakpoint>('lg')
  const visibleIds = visibleTilesFor(bp, hidden, mode)

  // RGL filters layouts to only visible items so missing entries don't crash.
  const filteredLayouts: ResponsiveLayouts = {
    lg: layouts.lg.filter((l) => visibleIds.includes(l.i as TileId)),
    md: layouts.md.filter((l) => visibleIds.includes(l.i as TileId)),
    sm: layouts.sm.filter((l) => visibleIds.includes(l.i as TileId)),
    xs: layouts.xs.filter((l) => visibleIds.includes(l.i as TileId)),
  }

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center gap-2 px-4 py-2 border-b border-border bg-muted/20">
        <h1 className="text-sm font-semibold tracking-wide uppercase text-muted-foreground">{t('nav.game')}</h1>
        <div className="ml-auto flex items-center gap-2">
          <OverlayToggle />
          <AddTileMenu bp={bp} />
          <Button variant="ghost" size="sm" onClick={reset} className="text-xs">
            {t('common.reset_layout')}
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => setHelpOpen(true)}
            aria-label={t('game.onboarding.help_aria')}
            title={t('game.onboarding.help_aria')}
          >
            <HelpCircle className="size-4" />
          </Button>
        </div>
      </div>

      <DashboardOnboardingDialog open={helpOpen} onOpenChange={handleHelpOpenChange} />

      <AutoJoinPanel />

      <div ref={containerRef} className="flex-1 overflow-auto">
        {mounted && (
          <Responsive
            width={width}
            breakpoints={BREAKPOINTS}
            cols={COLS}
            rowHeight={30}
            margin={[12, 12]}
            containerPadding={[16, 16]}
            layouts={filteredLayouts}
            dragConfig={{ handle: '.tile-drag-handle' }}
            resizeConfig={{ handles: ['se'] }}
            compactor={verticalCompactor}
            onBreakpointChange={(b: string) => setBp(b as Breakpoint)}
            onLayoutChange={(_current: Layout, all: ResponsiveLayouts) => {
              const merged = mergeLayouts(layouts, all)
              setLayouts(merged)
            }}
          >
            {visibleIds.map((id) => (
              <div key={id} className="overflow-hidden">
                {renderTile(id, bp)}
              </div>
            ))}
          </Responsive>
        )}
      </div>
    </div>
  )
}

function AutoJoinPanel() {
  const { t } = useTranslation()
  const [status, setStatus] = useState<AutoJoinStatus | null>(null)

  useEffect(() => {
    let alive = true
    const refresh = () => {
      invoke<AutoJoinStatus>('get_auto_join_status')
        .then((next) => { if (alive) setStatus(next) })
        .catch(() => {})
    }
    refresh()
    const timer = window.setInterval(refresh, 1_000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [])

  const phase = status?.phase ?? 'disabled'
  const games = status?.max_games == null
    ? `${status?.completed_games ?? 0} / ∞`
    : `${status.completed_games} / ${status.max_games}`
  const time = status?.remaining_seconds == null
    ? '∞'
    : formatRemaining(status.remaining_seconds)

  return (
    <Card className="mx-4 mt-3 shrink-0">
      <CardContent className="flex flex-wrap items-center gap-x-6 gap-y-2 py-3 text-sm">
        <div className="flex items-center gap-2 font-medium">
          <span className={`size-2 rounded-full ${status?.running ? 'bg-emerald-500' : 'bg-zinc-500'}`} />
          {t('game.auto_join.title')}: {t(`game.auto_join.phase_${phase}`)}
        </div>
        <div className="flex items-center gap-2 text-muted-foreground">
          <Gamepad2 className="size-4" />
          {t('game.auto_join.games')}: <span className="font-mono text-foreground">{games}</span>
          {status?.remaining_games != null && ` (${t('game.auto_join.remaining')} ${status.remaining_games})`}
        </div>
        <div className="flex items-center gap-2 text-muted-foreground">
          <Clock3 className="size-4" />
          {t('game.auto_join.time')}: <span className="font-mono text-foreground">{time}</span>
        </div>
        {status?.stop_reason && (
          <div className="text-amber-500">
            {t(`game.auto_join.reason_${status.stop_reason}`)}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function formatRemaining(seconds: number) {
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  const secs = seconds % 60
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, '0')}:${String(secs).padStart(2, '0')}`
    : `${minutes}:${String(secs).padStart(2, '0')}`
}

// RGL only emits layouts for currently rendered (visible) tiles. Merge with
// the existing store so hidden entries keep their last known position.
function mergeLayouts(prev: Record<Breakpoint, LayoutItem[]>, next: ResponsiveLayouts): Record<Breakpoint, LayoutItem[]> {
  const out: Record<Breakpoint, LayoutItem[]> = { ...prev }
  for (const bp of ['lg', 'md', 'sm', 'xs'] as const) {
    const incoming = next[bp]
    if (!incoming) continue
    const incomingIds = new Set(incoming.map((l) => l.i))
    const kept = (prev[bp] ?? []).filter((l) => !incomingIds.has(l.i))
    out[bp] = [...incoming, ...kept]
  }
  return out
}
