import { useMemo, useRef, type RefObject } from 'react'
import { useTranslation } from 'react-i18next'
import { TileFrame } from '@/components/TileFrame'
import { Mahgen } from '@/components/Mahgen'
import { useGameStore } from '@/stores/gameStore'
import { useAnalysisStore } from '@/stores/analysisStore'
import { tileIdx, mjaiToMahgen, sortMjaiTiles } from '@/lib/tileIdx'
import type { Breakpoint } from '@/tiles/defaults'

// Deal-in risk, but only for the tiles in our own hand — laid out like the Self
// Hand panel (suit order) with each tile annotated by its mixed-risk value. The
// full 34-tile chart was noise; what matters when choosing a discard is how
// dangerous each tile *you actually hold* is.
export function RiskChartTile({ bp }: { bp: Breakpoint }) {
  const { t } = useTranslation()
  const game = useGameStore((s) => s.game)
  const risk = useAnalysisStore((s) => s.result?.mixed_risk ?? null)
  // Mahgen sizes off this ref (the content row), so every tile shares one height
  // and the strip wraps when the panel is narrow.
  const rowRef = useRef<HTMLDivElement>(null)

  const ourSeat = game?.our_seat ?? null
  const sorted = useMemo(() => {
    const tehai = ourSeat != null ? game?.players[ourSeat]?.tehai ?? [] : []
    return sortMjaiTiles(tehai)
  }, [game, ourSeat])

  return (
    <TileFrame id="risk-chart" title={t('tile.risk_chart')} bp={bp}>
      {sorted.length === 0 ? (
        <div className="flex h-full items-center justify-center">
          <span className="text-muted-foreground text-sm">{t('tile.risk_chart_empty')}</span>
        </div>
      ) : (
        <div
          ref={rowRef}
          className="flex flex-wrap content-start items-start justify-center gap-x-2 gap-y-3"
        >
          {sorted.map((tile, i) => {
            const idx = tileIdx(tile)
            const v = risk && idx >= 0 ? risk[idx] : null
            return <RiskTile key={i} tile={tile} value={v} rowRef={rowRef} />
          })}
        </div>
      )}
    </TileFrame>
  )
}

// Risk tiers reuse the original RiskChartTile cutoffs (>=20 red, >=10 amber,
// else emerald). Each tier maps to a colored glow ring around the tile plus a
// matching badge tint; foreground colors are split light/dark so the numbers
// stay legible in both themes. The glow is an inline box-shadow rather than an
// arbitrary `shadow-[...]` class: Tailwind's scanner drops multi-layer arbitrary
// shadows (the commas break candidate extraction), so the class would be purged.
type Tier = { glow: string; badge: string }

function riskTier(v: number | null): Tier {
  if (v == null) return { glow: 'none', badge: 'bg-muted text-muted-foreground' }
  if (v >= 20)
    return {
      glow: '0 0 0 2px rgba(239,68,68,0.75), 0 0 10px 2px rgba(239,68,68,0.55)',
      badge: 'bg-red-500/15 text-red-600 dark:text-red-400',
    }
  if (v >= 10)
    return {
      glow: '0 0 0 2px rgba(245,158,11,0.75), 0 0 10px 2px rgba(245,158,11,0.5)',
      badge: 'bg-amber-500/15 text-amber-600 dark:text-amber-400',
    }
  return {
    glow: '0 0 0 2px rgba(16,185,129,0.55), 0 0 8px 1px rgba(16,185,129,0.4)',
    badge: 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400',
  }
}

function RiskTile({
  tile,
  value,
  rowRef,
}: {
  tile: string
  value: number | null
  rowRef: RefObject<HTMLDivElement | null>
}) {
  const tier = riskTier(value)
  return (
    <div className="flex flex-col items-center gap-1">
      <div className="rounded-[3px] leading-none" style={{ boxShadow: tier.glow }}>
        <Mahgen seq={mjaiToMahgen([tile])} kind="hand-risk" containerRef={rowRef} />
      </div>
      <span
        className={`min-w-[2.4em] rounded px-1 py-0.5 text-center text-[10px] font-mono tabular-nums leading-none ${tier.badge}`}
      >
        {value == null ? '—' : value.toFixed(1)}
      </span>
    </div>
  )
}
