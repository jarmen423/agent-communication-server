// State/model: shared constants, collections and mutable session state (S).
export const agents = new Map();
export const envelopes = [];
export const transmissions = [];
export const particleEffects = [];
export const broadcastRipples = [];
export const optimisticChatHtml = [];
export const S = {
  ws: null, wsConnectedAt: 0, wsReconnecting: false,
  selectedAgent: null, hoveredAgent: null, totalMessageCount: 0,
  pointerGesture: null, sceneBgImage: null, sceneBgActive: false,
  openLogAgent: null,
};
export const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
export const COLORS = {
  bg: [6, 6, 10],
  grid: [28, 32, 52],
  gridAccent: [40, 55, 80],
  agent: {
    working: [0, 255, 136],
    idle: [90, 100, 130],
    error: [255, 42, 95],
    ready: [0, 217, 255],
    thinking: [255, 170, 0],
    stopped: [58, 62, 78],
  },
  text: [232, 237, 245],
  bubbleBg: [12, 14, 22],
};

export const STATUS_LABELS = {
  working: 'WORKING',
  thinking: 'THINKING',
  ready: 'READY',
  error: 'ERROR',
  idle: 'IDLE',
  stopped: 'STOPPED',
};


export const MAX_PARTICLES = 80;
export const MAX_BUBBLE_LINES = 3;
export const BUBBLE_MAX_W = 200;


// Sprite grab / move (click = menu; hold or drag = reposition, no menu)
export const AGENT_HOLD_MS = 220;
export const AGENT_DRAG_PX = 7;
export const POS_STORAGE_KEY = 'nats-hub.positions';
export const BG_STORAGE_KEY = 'nats-hub.scene-bg';

export function prettySnippet(text) {
  if (!text || typeof text !== 'string') return text;
  let s = text.trim().replace(/\s+/g, ' ');

  // Common echo payload: "echo: <reversed prompt>"
  if (/^echo:\s*/i.test(s)) {
    const body = s.replace(/^echo:\s*/i, '');
    return 'echo ← ' + body.split('').reverse().join('');
  }

  // Heuristic: reversed English from echo_worker ("ksat", "troper", "ezylana")
  if (/(ksat|troper|ezylana|suounitno|sgnidnif|eludom)/i.test(s)) {
    return s.split('').reverse().join('');
  }
  return s;
}

export function agentGlyph(identity) {
  const id = (identity || '').toLowerCase();
  if (id.includes('hermes')) return '⚡';
  if (id.includes('cursor')) return '◇';
  if (id.includes('agy')) return '◈';
  if (id.includes('josh') || id.includes('human')) return '★';
  return '■';
}

export function agentPalette(identity) {
  let h = 0;
  for (let i = 0; i < identity.length; i++) h = (h * 31 + identity.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  const s = 0.80, l = 0.60; // Vibrant pastel neon ranges
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs((hue / 60) % 2 - 1));
  const m = l - c / 2;
  let r = 0, g = 0, b = 0;
  if (hue < 60) { r = c; g = x; }
  else if (hue < 120) { r = x; g = c; }
  else if (hue < 180) { g = c; b = x; }
  else if (hue < 240) { g = x; b = c; }
  else if (hue < 300) { r = x; b = c; }
  else { r = c; b = x; }
  return [Math.round((r + m) * 255), Math.round((g + m) * 255), Math.round((b + m) * 255)];
}

export function showToast(msg, isError) {
  const el = document.getElementById('toast');
  if (!el) return;
  el.textContent = msg;
  el.style.borderColor = isError ? 'var(--color-red)' : 'var(--color-green)';
  el.style.color = isError ? 'var(--color-red)' : 'var(--color-green)';
  el.classList.add('visible');
  el.style.display = 'block';
  clearTimeout(showToast._t);
  showToast._t = setTimeout(() => {
    el.classList.remove('visible');
    el.style.display = 'none';
  }, 3200);
}

export function formatUptime(ms) {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function extractEnvelopeBody(payload) {
  if (!payload || typeof payload !== 'object') return '';
  const data = (payload.data && typeof payload.data === 'object') ? payload.data : null;

  // Nested stream/progress payloads (Grok ACP mid-turn)
  if (data) {
    const nested =
      data.message || data.text || data.result || data.error ||
      data.prompt || data.delta || null;
    if (nested != null && String(nested).trim()) {
      const phase = data.phase ? `[${data.phase}] ` : '';
      return phase + String(nested);
    }
  }

  // Top-level common fields
  const top =
    payload.result || payload.message || payload.error || payload.prompt ||
    payload.status || null;
  if (top != null && String(top).trim() && String(top) !== payload.event_type) {
    return String(top);
  }

  // Event type with tiny summary
  if (payload.event_type) {
    if (data && Object.keys(data).length) {
      try {
        const s = JSON.stringify(data);
        if (s.length < 200) return `${payload.event_type}: ${s}`;
      } catch (_) {}
      return String(payload.event_type);
    }
    return String(payload.event_type);
  }

  try {
    return JSON.stringify(payload).substring(0, 240);
  } catch (_) {
    return '';
  }
}

export function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

