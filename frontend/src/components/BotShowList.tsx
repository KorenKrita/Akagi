import type { RefObject } from 'react'
import { Mahgen } from '@/components/Mahgen'
import { hexToRgba } from '@/lib/botShow'
import { mjaiToMahgen } from '@/lib/tileIdx'
import { cn } from '@/lib/utils'
import type { ShowItem } from '@/types'

// The bot's `meta.show` rows, rendered once and used twice: by the Bot Show
// dashboard tile and by the always-on-top overlay window. Purely presentational
// — see `@/lib/botShow` for the helpers that turn a `bot-response` into rows.

type Props = {
  items: ShowItem[]
  /** Element whose width drives mahgen tile sizing. */
  containerRef: RefObject<HTMLOListElement | null>
  /** Tighter padding and smaller type, for the overlay's small window. */
  dense?: boolean
  className?: string
}

export function BotShowList({ items, containerRef, dense, className }: Props) {
  return (
    <ol ref={containerRef} className={cn('flex flex-col', dense ? 'gap-0.5' : 'gap-1', className)}>
      {items.map((it, i) => {
        const seq = it.tiles ?? (it.pais ? mjaiToMahgen(it.pais) : '')
        return (
          <li
            key={i}
            className={cn(
              'flex items-center rounded-md border border-border',
              dense ? 'gap-1.5 px-1.5 py-1' : 'gap-2 px-2 py-1.5',
            )}
            style={{
              backgroundColor: hexToRgba(it.color, 0.1),
              borderLeftColor: it.color,
              borderLeftWidth: it.color ? 3 : undefined,
            }}
          >
            {seq && <Mahgen seq={seq} kind="bot-show" containerRef={containerRef} />}
            <div className="flex flex-col flex-1 min-w-0">
              {it.label && (
                <span className={cn('text-foreground truncate', dense ? 'text-xs' : 'text-sm')}>
                  {it.label}
                </span>
              )}
              {it.note && (
                <span className="text-[10px] text-muted-foreground truncate">{it.note}</span>
              )}
            </div>
            {it.value && (
              <span
                className={cn(
                  'font-mono tabular-nums text-foreground/90',
                  dense ? 'text-[11px]' : 'text-xs',
                )}
              >
                {it.value}
              </span>
            )}
          </li>
        )
      })}
    </ol>
  )
}
