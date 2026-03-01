import { X } from 'lucide-react';
import type { PointerEvent } from 'react';
import { useRef } from 'react';

import StreamPlayer from '@/components/StreamPlayer';
import { Button } from '@/components/ui/button';
import { ModelStatusIndicator } from '@/components/ui/model-status-indicator';
import { HUD_MAX_WIDTH, HUD_MIN_WIDTH } from '@/config/constants';

export default function Hud() {
  const startPos = useRef<{ x: number; w: number; active: boolean }>({
    x: 0,
    w: 0,
    active: false,
  });
  const rafId = useRef<number | null>(null);
  const pendingBounds = useRef<{ width: number; height: number } | null>(null);

  const handlePointerDown = (e: PointerEvent) => {
    e.preventDefault();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);

    startPos.current = {
      x: e.screenX,
      w: window.innerWidth,
      active: true,
    };
    document.body.style.cursor = 'nwse-resize';
  };

  const handlePointerMove = (e: PointerEvent) => {
    if (!startPos.current.active) return;

    // 计算新尺寸
    const deltaX = e.screenX - startPos.current.x;
    const width = Math.min(HUD_MAX_WIDTH, Math.max(HUD_MIN_WIDTH, startPos.current.w + deltaX));
    // 强制 16:9 比例
    const height = Math.round((width * 9) / 16);

    pendingBounds.current = { width, height };
    if (rafId.current === null) {
      rafId.current = requestAnimationFrame(() => {
        rafId.current = null;
        if (pendingBounds.current) {
          window.electron.invoke('set-window-bounds', pendingBounds.current);
        }
      });
    }
  };

  const handlePointerUp = (e: PointerEvent) => {
    if (!startPos.current.active) return;

    startPos.current.active = false;
    document.body.style.cursor = '';
    pendingBounds.current = null;
    if (rafId.current !== null) {
      cancelAnimationFrame(rafId.current);
      rafId.current = null;
    }

    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      // 忽略
    }
  };

  return (
    <div className='draggable group relative flex h-screen w-full items-center justify-center overflow-hidden bg-transparent'>
      <StreamPlayer className='h-full w-full' />

      {/* Model Status Indicator */}
      <ModelStatusIndicator className='top-3 left-3' />

      {/* Close Button */}
      <div className='no-drag absolute top-2 right-2 z-60 opacity-40 transition-opacity hover:opacity-100'>
        <Button
          variant='ghost'
          size='icon'
          className='h-6 w-6 rounded-full bg-transparent text-white hover:bg-white/20 dark:text-zinc-200 dark:hover:bg-zinc-800/50'
          onClick={() => window.electron.invoke('toggle-hud', false)}
        >
          <X className='h-4 w-4' />
        </Button>
      </div>

      {/* Resize Handle */}
      <div className='no-drag absolute right-1 bottom-1 z-60 opacity-40 transition-opacity hover:opacity-100'>
        <Button
          variant='ghost'
          size='icon'
          className='h-6 w-6 cursor-nwse-resize rounded-full bg-transparent text-white hover:bg-white/20 dark:text-zinc-200 dark:hover:bg-zinc-800/50'
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp} // 安全兜底
        >
          <svg
            width='12'
            height='12'
            viewBox='0 0 24 24'
            fill='none'
            stroke='currentColor'
            strokeWidth='2'
            strokeLinecap='round'
          >
            <line x1='22' y1='10' x2='10' y2='22' />
            <line x1='22' y1='16' x2='16' y2='22' />
          </svg>
        </Button>
      </div>
    </div>
  );
}
