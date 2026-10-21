// Theme layer: original procedural agent sprites (replaces Petdex sheets).
// A theme is a manifest (themes/<name>/theme.json) listing variants with a
// shape + palette. Rendering is done with p5 primitives, so a theme needs
// no image assets and ships no third-party artwork. See docs/THEMES.md.
const FALLBACK = {
  name: 'default',
  cell: { width: 64, height: 70, hitRadius: 27, labelOffset: 41 },
  statusAnimations: {
    idle: { loopMs: 1100, bobAmp: 2.5, bobHz: 0.0016 },
    run: { loopMs: 620, bobAmp: 4.5, bobHz: 0.004 },
    review: { loopMs: 900, bobAmp: 2, bobHz: 0.002, tilt: 0.08 },
    failed: { loopMs: 1300, bobAmp: 1, bobHz: 0.001, shake: 1.6 },
    waiting: { loopMs: 1400, bobAmp: 1.5, bobHz: 0.0012 },
  },
  variants: [
    { id: 'orb-bot', label: 'Orb Bot', shape: 'orb', body: '#2dd4bf', accent: '#99f6e4', eye: '#06281f' },
    { id: 'hex-bot', label: 'Hex Bot', shape: 'hex', body: '#f59e0b', accent: '#fde68a', eye: '#2a1500' },
    { id: 'cat-bot', label: 'Cat Bot', shape: 'cat', body: '#38bdf8', accent: '#bae6fd', eye: '#082f49' },
    { id: 'ghost-bot', label: 'Ghost Bot', shape: 'ghost', body: '#a3a380', accent: '#e7e5c8', eye: '#26261a' },
  ],
};
const EXPLICIT = new Map();
export const Theme = {
  manifest: FALLBACK,
  VARIANTS: FALLBACK.variants.map((v) => v.id),
  async load(name = 'default') {
    try {
      const res = await fetch(`themes/${name}/theme.json`);
      if (res.ok) {
        const m = await res.json();
        if (m && Array.isArray(m.variants) && m.variants.length) {
          Theme.manifest = m;
          Theme.VARIANTS = m.variants.map((v) => v.id);
        }
      }
    } catch (_) { /* offline/file fallback: embedded manifest */ }
    return Theme.manifest;
  },
  preloadAll() { Theme.load(); },
  variant(id) {
    return Theme.manifest.variants.find((v) => v.id === id) || Theme.manifest.variants[0];
  },
  resolveVariant(identity) {
    const id = String(identity || '').toLowerCase();
    if (EXPLICIT.has(id)) return EXPLICIT.get(id);
    let h = 0;
    for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
    return Theme.VARIANTS[h % Theme.VARIANTS.length];
  },
  setExplicitVariant(identity, variant) {
    if (identity && variant) EXPLICIT.set(String(identity).toLowerCase(), variant);
  },
  statusToAnim(status, stopped) {
    if (stopped) return 'idle';
    if (status === 'error') return 'failed';
    if (status === 'working') return 'run';
    if (status === 'thinking') return 'review';
    return 'idle';
  },
  metrics() {
    const c = Theme.manifest.cell || FALLBACK.cell;
    return { width: c.width, height: c.height, hitRadius: c.hitRadius, labelOffset: c.labelOffset };
  },
  hitRadius() { return Theme.metrics().hitRadius; },
  // Draw the themed sprite for an agent. Always succeeds (procedural).
  drawAgent(agent) {
    const v = Theme.variant(agent.petSlug || Theme.resolveVariant(agent.identity));
    const animName = Theme.statusToAnim(agent.status, agent.stopped);
    const anim = (Theme.manifest.statusAnimations || FALLBACK.statusAnimations)[animName]
      || FALLBACK.statusAnimations.idle;
    const t = millis();
    const bob = Math.sin(t * (anim.bobHz || 0.0016) + (agent.bobOffset || 0)) * (anim.bobAmp || 2);
    const shake = anim.shake ? Math.sin(t * 0.03) * anim.shake : 0;
    const x = agent.x + shake;
    const y = agent.y + bob;
    const w = Theme.metrics().width;
    const h = Theme.metrics().height;
    push();
    translate(x, y);
    if (anim.tilt) rotate(Math.sin(t * 0.002) * anim.tilt);
    drawBody(v, w, h, animName, t, agent.stopped);
    pop();
    return true;
  },
};
function drawBody(v, w, h, animName, t, stopped) {
  const body = color(v.body);
  const accent = color(v.accent);
  const eye = color(v.eye);
  if (stopped) { body.setAlpha(110); accent.setAlpha(110); }
  stroke(accent); strokeWeight(2); fill(body);
  const hw = w / 2, hh = h / 2;
  if (v.shape === 'hex') {
    beginShape();
    for (let i = 0; i < 6; i++) {
      const a = (Math.PI / 3) * i - Math.PI / 2;
      vertex(Math.cos(a) * hw, Math.sin(a) * hh);
    }
    endShape(CLOSE);
  } else if (v.shape === 'ghost') {
    arc(0, -hh + w / 2, w, w, Math.PI, 0, PIE);
    rect(-hw, -hh + w / 2, w, hh - w / 2 + 8, 0, 0, 6, 6);
  } else {
    ellipse(0, 0, w, h * 0.82);
    if (v.shape === 'cat') {
      triangle(-hw + 6, -hh + 16, -hw + 20, -hh - 6, -hw + 26, -hh + 14);
      triangle(hw - 6, -hh + 16, hw - 20, -hh - 6, hw - 26, -hh + 14);
    }
    if (v.shape === 'orb') {
      line(0, -hh + 4, 0, -hh - 8);
      fill(accent); circle(0, -hh - 10, 7);
    }
  }
  // Eyes: blink by closing briefly every ~3s; failed = X eyes.
  fill(eye); noStroke();
  const blink = (t % 3000) < 120;
  if (animName === 'failed') {
    drawX(-10, -4); drawX(10, -4);
  } else if (blink) {
    rect(-14, -5, 8, 2); rect(6, -5, 8, 2);
  } else {
    circle(-10, -4, 7); circle(10, -4, 7);
  }
  // Feet / tread
  fill(accent);
  const step = animName === 'run' ? Math.sin(t * 0.02) * 4 : 0;
  rect(-16 + step, hh - 8, 12, 5, 2); rect(4 - step, hh - 8, 12, 5, 2);
}
function drawX(cx, cy) {
  stroke(30, 30, 30); strokeWeight(2);
  line(cx - 4, cy - 4, cx + 4, cy + 4); line(cx - 4, cy + 4, cx + 4, cy - 4);
  noStroke();
}
