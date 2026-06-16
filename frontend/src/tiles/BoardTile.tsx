import { useEffect, useRef, useState, type RefObject } from 'react'
import { useTranslation } from 'react-i18next'
import { TileFrame } from '@/components/TileFrame'
import { Mahgen } from '@/components/Mahgen'
import { useGameStore } from '@/stores/gameStore'
import { fmtScore, bakazeFor, kyokuLabel, relativeKind, type RelativeKind } from '@/lib/format'
import type { Breakpoint } from '@/tiles/defaults'
import './BoardTile.css'

// Screen edge each seat is drawn on, and the rotation applied to its tile
// layer (paint-only — see BoardTile.css for why sizing survives rotation).
type Edge = 'bottom' | 'right' | 'top' | 'left'
const EDGE_ROT: Record<Edge, number> = { bottom: 0, right: 270, top: 180, left: 90 }
const KIND_EDGE: Record<RelativeKind, Edge> = {
  self: 'bottom',
  shimocha: 'right',
  toimen: 'top',
  kamicha: 'left',
}
// Spectator fallback: with no hero seat, relativeKind() collapses everyone to
// 'self', so place seats by absolute index instead.
const IDENTITY_EDGE: Edge[] = ['bottom', 'right', 'top', 'left']

function seatEdge(seat: number, ourSeat: number | null, numPlayers: number): Edge {
  if (ourSeat == null) return IDENTITY_EDGE[seat] ?? 'bottom'
  return KIND_EDGE[relativeKind(seat, ourSeat, numPlayers)]
}

// Largest square that fits the (possibly non-square) content box. The pixel
// size is set on `.board-square` so every CSS percentage inside resolves
// against a known square and mahgen containers report stable widths.
function useSquareSize(ref: RefObject<HTMLElement | null>): number {
  const [size, setSize] = useState(0)
  useEffect(() => {
    const el = ref.current
    if (!el || typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect()
      setSize(Math.max(0, Math.floor(Math.min(r.width, r.height))))
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [ref])
  return size
}

export function BoardTile({ bp }: { bp: Breakpoint }) {
  const { t } = useTranslation()
  const game = useGameStore((s) => s.game)
  const view = useGameStore((s) => s.view)
  const rootRef = useRef<HTMLDivElement>(null)
  const size = useSquareSize(rootRef)

  const numPlayers = game?.num_players ?? view?.num_players ?? 4
  const ourSeat = game?.our_seat ?? null

  return (
    <TileFrame id="board" title={t('tile.board')} bp={bp} contentClassName="p-0">
      <div ref={rootRef} className="board-root">
        {game && view && size > 0 ? (
          <div className="board-square" style={{ width: size, height: size }}>
            {/* center: round / sticks / dora */}
            <div className="board-center">
              <div className="board-center-round">{kyokuLabel(game.bakaze, game.kyoku)}</div>
              <div className="board-center-sticks">
                <span className="board-stick">
                  <img src="/1000_mini.svg" alt="" />×{game.kyotaku}
                </span>
                <span className="board-stick">
                  <img src="/100_mini.svg" alt="" />×{game.honba}
                </span>
              </div>
              {view.dora_indicators && (
                <div className="board-center-dora">
                  <Mahgen seq={view.dora_indicators} kind="dora" />
                </div>
              )}
            </div>

            {/* rotated tile layer per seat */}
            {Array.from({ length: numPlayers }, (_, seat) => {
              const pview = view.players[seat]
              if (!pview) return null
              const edge = seatEdge(seat, ourSeat, numPlayers)
              return (
                <div
                  key={seat}
                  className="board-seat"
                  style={{ transform: `rotate(${EDGE_ROT[edge]}deg)` }}
                >
                  <div className="board-seat-stack">
                    {pview.river && (
                      <div className="board-seat-river">
                        <Mahgen seq={pview.river} kind="board-river" riverMode />
                      </div>
                    )}
                    {pview.melds.length > 0 && (
                      <div className="board-seat-melds">
                        {pview.melds.map((m, i) => (
                          <Mahgen key={i} seq={m} kind="board-meld" />
                        ))}
                      </div>
                    )}
                    {pview.hand && (
                      <div className="board-seat-hand">
                        <Mahgen seq={pview.hand} kind="board-hand" />
                      </div>
                    )}
                  </div>
                </div>
              )
            })}

            {/* upright nameplates per seat (current turn highlighted) */}
            {Array.from({ length: numPlayers }, (_, seat) => {
              const player = game.players[seat]
              if (!player) return null
              const edge = seatEdge(seat, ourSeat, numPlayers)
              const active = game.current_player === seat
              const kita = player.kita_tiles?.length ?? 0
              return (
                <div
                  key={seat}
                  className={`board-label board-label--${edge}${active ? ' board-label--active' : ''}`}
                >
                  <span className="board-label-wind">{bakazeFor(seat, game.oya, numPlayers)}</span>
                  <span className="board-label-score">{fmtScore(player.score)}</span>
                  {player.riichi_declared && (
                    <span className="board-badge board-badge--riichi">{t('mahjong.riichi')}</span>
                  )}
                  {kita > 0 && <span className="board-badge board-badge--kita">北×{kita}</span>}
                </div>
              )
            })}
          </div>
        ) : (
          <span className="board-empty">{t('tile.board_empty')}</span>
        )}
      </div>
    </TileFrame>
  )
}
