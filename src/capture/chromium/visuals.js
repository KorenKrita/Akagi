(() => {
  if (window.__akagiGameVisuals) return;

  const state = { risk: [], recommendation: null };
  let root;

  const canvas = () => [...document.querySelectorAll('canvas')]
    .sort((a, b) => b.clientWidth * b.clientHeight - a.clientWidth * a.clientHeight)[0];

  const colour = (risk) => {
    const stops = [[0, 59, 130, 246], [5, 34, 197, 94], [10, 245, 158, 11], [16, 239, 68, 68]];
    const hi = stops.findIndex((s) => risk <= s[0]);
    if (hi <= 0) return `rgb(${stops[0].slice(1).join(',')})`;
    if (hi < 0) return `rgb(${stops.at(-1).slice(1).join(',')})`;
    const a = stops[hi - 1], b = stops[hi], t = (risk - a[0]) / (b[0] - a[0]);
    return `rgb(${a.slice(1).map((v, i) => Math.round(v + (b[i + 1] - v) * t)).join(',')})`;
  };

  const render = () => {
    const c = canvas();
    if (!c || !document.body) return;
    const rect = c.getBoundingClientRect();
    if (!root) {
      root = document.createElement('div');
      root.id = 'akagi-game-visuals';
      Object.assign(root.style, { position: 'fixed', pointerEvents: 'none', zIndex: '2147483647' });
      document.body.appendChild(root);
    }
    Object.assign(root.style, {
      left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`
    });
    root.replaceChildren();

    for (const slot of state.risk) {
      const el = document.createElement('div');
      const c = colour(slot.risk);
      Object.assign(el.style, {
        position: 'absolute', left: `${slot.x / 16 * 100}%`, top: `${slot.y / 9 * 100}%`,
        width: '4.55%', height: '11.7%', transform: 'translate(-50%, -50%)',
        boxSizing: 'border-box', border: `4px solid ${c}`, borderRadius: '7px',
        background: `color-mix(in srgb, ${c} 16%, transparent)`, boxShadow: `0 0 8px ${c}`
      });
      root.appendChild(el);
    }

    if (state.recommendation) {
      const mark = document.createElement('div');
      mark.textContent = state.recommendation.label || 'AI';
      Object.assign(mark.style, {
        position: 'absolute', left: `${state.recommendation.x / 16 * 100}%`,
        top: `${(state.recommendation.y - 0.72) / 9 * 100}%`, transform: 'translate(-50%, -100%)',
        padding: '3px 8px', borderRadius: '5px', color: 'white', background: '#16a34a',
        border: '2px solid #86efac', boxShadow: '0 0 10px #22c55e',
        font: '700 14px/1.2 system-ui, sans-serif', whiteSpace: 'nowrap'
      });
      root.appendChild(mark);
    }
  };

  window.__akagiGameVisuals = {
    setRisk(value) { state.risk = Array.isArray(value) ? value : []; requestAnimationFrame(render); },
    clearRisk() { state.risk = []; requestAnimationFrame(render); },
    setRecommendation(value) { state.recommendation = value || null; requestAnimationFrame(render); },
    clearRecommendation() { state.recommendation = null; requestAnimationFrame(render); },
    clear() { state.risk = []; state.recommendation = null; requestAnimationFrame(render); }
  };
  addEventListener('resize', render, { passive: true });
})();
