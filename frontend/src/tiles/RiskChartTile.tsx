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

// Map deal-in risk (%) to a hue via a piecewise-linear ramp anchored to the
// deal-in thresholds that matter for defence. The curve is deliberately
// non-linear: mixed_risk is a deal-in % whose decision-critical band sits in
// ~0..16%, so a plain linear 0..20 map left genuinely dangerous tiles (~13%)
// looking yellow-green and almost never reached red. The anchors line up with
// the engine's own risk table (src/analysis/data/risk.rs): suji ~2-5%, no-suji
// 3/7 ~9.5%, no-suji 5 ~12.8%, with dora/late/ippatsu/multi-tenpai pushing 16%+.
// Colours are inline styles (not Tailwind classes) since the hue is per tile.
const RISK_HUE_ANCHORS: ReadonlyArray<readonly [risk: number, hue: number]> = [
  [0, 225], // blue  — genbutsu / guaranteed safe
  [5, 130], // green — suji, low risk
  [10, 45], // amber — no-suji 3/7, moderate
  [16, 0], //  red   — no-suji 5 + dora, late, ippatsu, multi-tenpai
]

function riskHue(v: number): number {
  const a = RISK_HUE_ANCHORS
  if (v <= a[0][0]) return a[0][1]
  for (let i = 1; i < a.length; i++) {
    const [v0, h0] = a[i - 1]
    const [v1, h1] = a[i]
    if (v <= v1) return h0 + ((h1 - h0) * (v - v0)) / (v1 - v0)
  }
  return a[a.length - 1][1] // clamp >= last anchor (16%) to full red
}

type RiskColor = { ring: string; glow: string; chip: string }

function riskColor(value: number | null): RiskColor {
  if (value == null) {
    // No analysis yet: neutral grey, no hue.
    return { ring: 'hsl(0 0% 55%)', glow: 'hsl(0 0% 55% / 0.4)', chip: 'hsl(0 0% 45%)' }
  }
  const hue = riskHue(value)
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
