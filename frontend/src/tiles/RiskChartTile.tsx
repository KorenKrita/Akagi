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

// Map deal-in risk to a smooth blue -> green -> yellow -> red colour (continuous,
// not bucketed). Hue runs linearly from 240 deg (blue, safest) down to 0 deg
// (red) over a 0..RISK_MAX range; values past RISK_MAX clamp to red. RISK_MAX is
// tuned so green lands near a ~10% deal-in and red near ~20%, lining up with the
// risk levels the old discrete cutoffs used. Colours are inline styles (not
// Tailwind classes) since the hue is computed per tile.
const RISK_MAX = 20

type RiskColor = { ring: string; glow: string; chip: string }

function riskColor(value: number | null): RiskColor {
  if (value == null) {
    // No analysis yet: neutral grey, no hue.
    return { ring: 'hsl(0 0% 55%)', glow: 'hsl(0 0% 55% / 0.4)', chip: 'hsl(0 0% 45%)' }
  }
  const t = Math.min(1, Math.max(0, value / RISK_MAX))
  const hue = 240 * (1 - t)
  return {
    ring: `hsl(${hue} 80% 55%)`,
    glow: `hsl(${hue} 85% 55% / 0.5)`,
    // Darker so white text on the chip stays legible across the whole gradient.
    chip: `hsl(${hue} 70% 40%)`,
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
  const c = riskColor(value)
  return (
    <div className="flex flex-col items-center gap-1">
      {/* flex + leading-0 so the wrapper shrink-wraps the <mah-gen> tile exactly
          (the host is display:inline, which otherwise reserves line-box space the
          ring would expose as a gap above the tile). */}
      <div
        className="flex rounded-[3px] leading-[0]"
        style={{ boxShadow: `0 0 0 2px ${c.ring}, 0 0 8px 2px ${c.glow}` }}
      >
        <Mahgen seq={mjaiToMahgen([tile])} kind="hand-risk" containerRef={rowRef} className="leading-[0]" />
      </div>
      <span
        className="min-w-[2.4em] rounded px-1 py-0.5 text-center text-[10px] font-mono tabular-nums leading-none text-white"
        style={{ backgroundColor: c.chip }}
      >
        {value == null ? '—' : value.toFixed(1)}
      </span>
    </div>
  )
}
