/**
 * Petdex sprite layer for the nats-hub arcade visualizer.
 * Geometry matches Hermes agent/pet/constants.py (192×208 cells, 6 frames/state).
 */
const Petdex = (() => {
  const FRAME_W = 192;
  const FRAME_H = 208;
  const FRAMES_PER_STATE = 6;
  const DISPLAY_SCALE = 0.34;

  const CODEX_STATE_ROWS = [
    'idle', 'running-right', 'running-left', 'waving', 'jumping',
    'failed', 'waiting', 'running', 'review',
  ];
  const LEGACY_STATE_ROWS = [
    'idle', 'wave', 'run', 'failed', 'review', 'jump', 'extra1', 'extra2',
  ];
  const STATE_ALIASES = {
    idle: ['idle'],
    wave: ['wave', 'waving'],
    run: ['run', 'running', 'running-right', 'running-left'],
    failed: ['failed'],
    review: ['review'],
    jump: ['jump', 'jumping'],
    waiting: ['waiting'],
  };
  const LOOP_MS = { idle: 1100, run: 620, review: 900, failed: 1300 };

  const INSTALLED_PETS = ['batmeme', 'jill-stingray', 'maisenpai', 'scoop'];
  const EXPLICIT_PET_MAP = {
    'petdex-agent': 'batmeme',
    'design-agent': 'jill-stingray',
    'surreal-agent': 'maisenpai',
  };

  const cache = new Map();

  function hashIdentityToPet(identity) {
    const id = (identity || '').toLowerCase();
    if (EXPLICIT_PET_MAP[id]) return EXPLICIT_PET_MAP[id];
    let h = 0;
    for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
    return INSTALLED_PETS[h % INSTALLED_PETS.length];
  }

  /** Map arcade agent status → petdex animation row name. */
  function agentStatusToPetState(status, stopped) {
    if (stopped) return 'idle';
    if (status === 'error') return 'failed';
    if (status === 'working' || status === 'ready') return status === 'working' ? 'run' : 'idle';
    if (status === 'thinking') return 'review';
    return 'idle';
  }

  function rowNames(sheetRows) {
    return sheetRows >= 9 ? CODEX_STATE_ROWS : LEGACY_STATE_ROWS;
  }

  function rowIndex(sheetRows, state) {
    const rows = rowNames(sheetRows);
    const aliases = STATE_ALIASES[state] || [state];
    for (const name of aliases) {
      const idx = rows.indexOf(name);
      if (idx >= 0 && idx < sheetRows) return idx;
    }
    return 0;
  }

  function hasStateRow(sheetRows, state) {
    const rows = rowNames(sheetRows);
    const aliases = STATE_ALIASES[state] || [state];
    for (const name of aliases) {
      const idx = rows.indexOf(name);
      if (idx >= 0 && idx < sheetRows) return true;
    }
    return false;
  }

  class PetSprite {
    constructor(slug) {
      this.slug = slug;
      this.sheet = null;
      this.loaded = false;
      this.failed = false;
      this.sheetRows = 9;
      loadImage(
        `pets/${slug}/spritesheet.webp`,
        (img) => {
          this.sheet = img;
          this.sheetRows = Math.max(1, Math.floor(img.height / FRAME_H));
          this.loaded = true;
        },
        () => { this.failed = true; }
      );
    }

    ready() {
      return this.loaded && !this.failed && this.sheet;
    }

    animFrame(state) {
      const loop = LOOP_MS[state] || LOOP_MS.idle;
      return Math.floor((millis() % loop) / (loop / FRAMES_PER_STATE)) % FRAMES_PER_STATE;
    }

    draw(x, y, state, opts) {
      if (!this.ready()) return false;

      const row = rowIndex(this.sheetRows, state);
      const col = this.animFrame(state);
      // p5 image(img, dx, dy, dw, dh, sx, sy, sw, sh) — sw/sh are SOURCE SIZE, not corner coords.
      // Passing sx+FRAME_W / sy+FRAME_H made later frames sample 2–3+ cells → “split pet” glitch.
      const sx = col * FRAME_W;
      const sy = row * FRAME_H;
      const sw = FRAME_W;
      const sh = FRAME_H;
      const w = FRAME_W * DISPLAY_SCALE;
      const h = FRAME_H * DISPLAY_SCALE;
      const bobAmp = state === 'run' ? 5.5 : state === 'review' ? 3.5 : 4;
      const bob = Math.sin(millis() * 0.003 + (opts.bobPhase || 0)) * bobAmp;

      push();
      imageMode(CENTER);
      if (opts.grayscale) {
        tint(140, 140, 155, 185);
      } else if (opts.errorTint) {
        tint(255, 90, 110, 255);
      } else {
        noTint();
      }
      image(this.sheet, x, y + bob, w, h, sx, sy, sw, sh);
      noTint();
      pop();
      return true;
    }
  }

  function getSprite(slug) {
    if (!cache.has(slug)) cache.set(slug, new PetSprite(slug));
    return cache.get(slug);
  }

  function preloadAll() {
    for (const slug of INSTALLED_PETS) getSprite(slug);
  }

  function resolveSlug(identity) {
    return hashIdentityToPet(identity);
  }

  function spriteReady(slug) {
    return getSprite(slug).ready();
  }

  function metrics() {
    const w = FRAME_W * DISPLAY_SCALE;
    const h = FRAME_H * DISPLAY_SCALE;
    return { width: w, height: h, hitRadius: h * 0.42, labelOffset: h * 0.48 + 6 };
  }

  /** Draw animated pet for an arcade agent. Returns false → caller uses chip fallback. */
  function drawAgent(agent) {
    const slug = hashIdentityToPet(agent.identity);
    const sprite = getSprite(slug);
    const state = agentStatusToPetState(agent.status, agent.stopped);
    const useErrorTint = agent.status === 'error' && !hasStateRow(sprite.sheetRows, 'failed');
    return sprite.draw(agent.x, agent.y, state, {
      bobPhase: agent.bobOffset,
      grayscale: agent.stopped,
      errorTint: useErrorTint,
    });
  }

  function hitRadius(slug) {
    return spriteReady(slug) ? metrics().hitRadius : 0;
  }

  return {
    INSTALLED_PETS,
    EXPLICIT_PET_MAP,
    resolveSlug,
    agentStatusToPetState,
    preloadAll,
    drawAgent,
    spriteReady,
    hitRadius,
    metrics,
  };
})();