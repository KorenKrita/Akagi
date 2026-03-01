import { useEffect } from 'react';

// prettier-ignore
const TILES = [
  '1m', '2m', '3m', '4m', '5m', '5mr', '6m', '7m', '8m', '9m',
  '1p', '2p', '3p', '4p', '5p', '5pr', '6p', '7p', '8p', '9p',
  '1s', '2s', '3s', '4s', '5s', '5sr', '6s', '7s', '8s', '9s',
  'E', 'S', 'W', 'N', 'P', 'F', 'C',
];

declare global {
  interface Window {
    __tiles_preloaded?: boolean;
  }
}

/**
 * 预加载麻将牌 SVG 的 Hook ，用于在应用初始加载时预加载麻将牌的SVG图片。
 */
export function useTilePreloader() {
  useEffect(() => {
    if (window.__tiles_preloaded) return;

    const preloadTile = (tile: string) => {
      const img = new Image();
      img.src = `Resources/${tile}.svg`;
    };

    // 使用 requestIdleCallback 避免阻塞主线程
    if (window.requestIdleCallback) {
      window.requestIdleCallback(() => {
        TILES.forEach(preloadTile);
      });
    } else {
      // Fallback
      const timeoutId = setTimeout(() => {
        TILES.forEach(preloadTile);
      }, 1000);
      return () => clearTimeout(timeoutId);
    }

    window.__tiles_preloaded = true;
  }, []);
}
