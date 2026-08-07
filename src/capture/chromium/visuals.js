(() => {
  if (window.__akagiGameVisuals) return;

  const SUITS = { '0.5': 'm', '0.25': 'p', '0.75': 's' };
  const HONORS = ['E', 'S', 'W', 'N', 'P', 'F', 'C'];
  const targets = new Map();
  const installed = new WeakSet();
  let recommendation = null;
  let marker;

  const tileFromST = (st) => {
    // Mahjong Soul Unity atlas mapping verified by Sunalamye/Naki.
    if (!st || st.length < 4) return null;
    // A tile occupies one cell in the 10x4 atlas. Generic Unity UI commonly
    // uses (1,1,0,0), whose zero offset would otherwise be mistaken for East.
    if (Math.abs(st[0] - .1) > .015 || Math.abs(st[1] - .25) > .015) return null;
    const u = Math.round(st[2] * 10) / 10;
    const v = Math.round(st[3] * 100) / 100;
    const suit = SUITS[String(v)];
    if (suit) {
      const n = Math.round(u * 10);
      return n === 0 ? `5${suit}r` : n >= 1 && n <= 9 ? `${n}${suit}` : null;
    }
    if (v === 0) return HONORS[Math.round(u * 10)] || null;
    return null;
  };

  const riskColour = (risk) => {
    const stops = [[0, .55, .72, 1], [5, .5, 1, .55], [10, 1, .78, .4], [16, 1, .4, .4]];
    let hi = stops.findIndex((s) => risk <= s[0]);
    if (hi < 0) return stops.at(-1).slice(1);
    if (hi === 0) return stops[0].slice(1);
    const a = stops[hi - 1], b = stops[hi], t = (risk - a[0]) / (b[0] - a[0]);
    return a.slice(1).map((v, i) => v + (b[i + 1] - v) * t);
  };

  const install = (gl) => {
    if (!gl || installed.has(gl)) return gl;
    installed.add(gl);

    const locations = new WeakMap();
    let program = gl.getParameter(gl.CURRENT_PROGRAM);
    let unit = (gl.getParameter(gl.ACTIVE_TEXTURE) || gl.TEXTURE0) - gl.TEXTURE0;
    const textures = [];
    textures[unit] = gl.getParameter(gl.TEXTURE_BINDING_2D);

    const originalUseProgram = gl.useProgram.bind(gl);
    gl.useProgram = (next) => { program = next; return originalUseProgram(next); };

    const originalActiveTexture = gl.activeTexture.bind(gl);
    gl.activeTexture = (next) => { unit = next - gl.TEXTURE0; return originalActiveTexture(next); };

    const originalBindTexture = gl.bindTexture.bind(gl);
    gl.bindTexture = (type, texture) => {
      if (type === gl.TEXTURE_2D) textures[unit] = texture;
      return originalBindTexture(type, texture);
    };

    const originalDrawElements = gl.drawElements.bind(gl);
    gl.drawElements = (mode, count, type, offset) => {
      if (!program || !textures[unit] || count !== 6 || !targets.size) return originalDrawElements(mode, count, type, offset);
      let loc = locations.get(program);
      if (loc === undefined) {
        const st = gl.getUniformLocation(program, '_MainTex_ST');
        // Mahjong Soul hand tiles use _Tint; _Color is shared by generic UI.
        const color = gl.getUniformLocation(program, '_Tint');
        loc = st && color ? { st, color } : null;
        locations.set(program, loc);
      }
      if (!loc) return originalDrawElements(mode, count, type, offset);

      let color;
      try { color = targets.get(tileFromST(gl.getUniform(program, loc.st))); } catch (_) {}
      if (!color) return originalDrawElements(mode, count, type, offset);

      const previous = gl.getUniform(program, loc.color);
      const alpha = previous && previous.length > 3 ? previous[3] : 1;
      gl.uniform4f(loc.color, color[0], color[1], color[2], alpha);
      const result = originalDrawElements(mode, count, type, offset);
      if (previous && previous.length > 3) gl.uniform4f(loc.color, previous[0], previous[1], previous[2], previous[3]);
      return result;
    };
    return gl;
  };

  const originalGetContext = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = function (type, options) {
    const context = originalGetContext.call(this, type, options);
    return type === 'webgl' || type === 'experimental-webgl' || type === 'webgl2' ? install(context) : context;
  };
  document.querySelectorAll('canvas').forEach((canvas) => install(canvas.getContext('webgl2') || canvas.getContext('webgl')));

  const renderMarker = () => {
    const canvas = [...document.querySelectorAll('canvas')]
      .sort((a, b) => b.clientWidth * b.clientHeight - a.clientWidth * a.clientHeight)[0];
    if (!canvas || !document.body || !recommendation) {
      if (marker) marker.remove();
      marker = null;
      return;
    }
    const rect = canvas.getBoundingClientRect();
    if (!marker) {
      marker = document.createElement('div');
      marker.id = 'akagi-recommendation-marker';
      document.body.appendChild(marker);
    }
    marker.textContent = recommendation.label || 'AI';
    Object.assign(marker.style, {
      position: 'fixed', pointerEvents: 'none', zIndex: '2147483647',
      left: `${rect.left + recommendation.x / 16 * rect.width}px`,
      top: `${rect.top + (recommendation.y - .72) / 9 * rect.height}px`,
      transform: 'translate(-50%, -100%)', padding: '3px 8px', borderRadius: '5px',
      color: 'white', background: '#16a34a', border: '2px solid #86efac',
      boxShadow: '0 0 10px #22c55e', font: '700 14px/1.2 system-ui, sans-serif'
    });
  };

  window.__akagiGameVisuals = {
    setRisk(values) {
      targets.clear();
      (values || []).forEach(({ tile, risk }) => targets.set(tile, riskColour(risk)));
    },
    clearRisk() { targets.clear(); },
    setRecommendation(value) { recommendation = value || null; requestAnimationFrame(renderMarker); },
    clearRecommendation() { recommendation = null; requestAnimationFrame(renderMarker); },
    clear() { targets.clear(); recommendation = null; requestAnimationFrame(renderMarker); },
    decode: tileFromST
  };
  addEventListener('resize', renderMarker, { passive: true });
})();
