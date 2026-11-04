// The ASCII web: the whole hero is a still field of protocol text, and the
// conversation shows only as brightness moving through it. A line joins two
// named agents through the hub (every message routes through hub-server). It
// lifts its characters slightly while alive; signals are bright pulses that
// travel it: one teal task out, a few small orange progress pulses back, one
// bigger orange result. Then the line dies and another lights up elsewhere.
// Everything here is analytic and deterministic, so the server poster
// (web-poster.tsx) and the live shader (web-cells.wgsl) compute the same light.
import { rng } from "./prng";

import type { Band } from "./web-bands";
import { CARD, PLACES, placeSteady, SLOTS, type Role, type Slot } from "./web-places";

export { BAND_QUERY, bandFor, cover, type Band } from "./web-bands";
export type WebAgent = { id: string; role: Role; x: number; y: number; place: string; slot: number; j: number };
export type WebLayout = {
  band: Band;
  variant: "wide" | "tall";
  /**
   * Design space, cover-mapped onto the hero. Desktop is 2400 × 1000: up to an
   * aspect of 2.4 the full height stays visible, so the safe band between the
   * header and the log strip is stable in design units; only the visible width
   * changes with aspect, which is what the bands are for.
   */
  width: number;
  height: number;
  hub: { cx: number; cy: number; size: number };
  plate: { x: number; y: number; w: number; h: number; r: number };
  grid: readonly [number, number];
  agents: WebAgent[];
  /** Location slots around the hub (see web-places.ts). */
  slots: Slot[];
  /** Seconds between new lines, and the line count cap. */
  every: readonly [number, number];
  maxLines: number;
  speed: number;
  /**
   * The copy block (+16 px) measured at this band's reference viewport (P 768 × 1024,
   * M 1440 × 900, W 1894 × 920, phone 390 × 844), design units: the poster's mask.
   * The live layer measures the real one.
   */
  copy: readonly [number, number, number, number];
};

const plateOf = (cx: number, cy: number, size: number, r: number) => {
  const half = size * 0.82;
  return { x: cx - half, y: cy - half, w: half * 2, h: half * 2, r };
};

/** Every agent of every place a band's slots can show, at its port (card rows run down from the title). */
function agentsOf(slots: Slot[], card: (typeof CARD)["wide" | "tall"]): WebAgent[] {
  return slots.flatMap((slot, i) =>
    slot.places.flatMap((pid, j) =>
      PLACES[pid].agents.slice(0, card.rows).map(([id, role], k) => ({ id, role, x: slot.x, y: slot.y + card.first + k * card.row, place: pid, slot: i, j })),
    ),
  );
}

const desktop = (band: "P" | "M" | "W", hub: [number, number, number], maxLines: number, copy: WebLayout["copy"]): WebLayout => ({
  band,
  variant: "wide",
  width: 2400,
  height: 1000,
  hub: { cx: hub[0], cy: hub[1], size: hub[2] },
  plate: plateOf(hub[0], hub[1], hub[2], 24),
  grid: [300, 71],
  every: maxLines > 4 ? [1.5, 2.2] : [2.2, 3],
  maxLines,
  speed: 1,
  agents: agentsOf(SLOTS[band], CARD.wide),
  slots: SLOTS[band],
  copy,
});

export const WEB: Record<Band, WebLayout> = {
  // Near-portrait screens (tablets, squarish windows): copy on top, the hub and its places below.
  P: desktop("P", [1200, 730, 84], 4, [840, 120, 1332, 558]),
  // Laptops: copy upper left, the hub upper right (about 78% across, 43% down), places around it.
  M: desktop("M", [1640, 430, 96], 5, [551, 133, 1442, 583]),
  // Wide and short screens: the same, with places also on the far left.
  W: desktop("W", [1780, 430, 100], 6, [565, 130, 1437, 570]),
  tall: {
    band: "tall",
    variant: "tall",
    width: 400,
    height: 860,
    hub: { cx: 200, cy: 610, size: 62 },
    plate: plateOf(200, 610, 62, 16),
    grid: [40, 50],
    every: [2.6, 3.4],
    maxLines: 3,
    speed: 0.5,
    agents: agentsOf(SLOTS.tall, CARD.tall),
    slots: SLOTS.tall,
    copy: [17, 80, 384, 495],
  },
};

// ---- lines ----

type Pt = [number, number];
export type Pulse = { kind: "task" | "progress" | "result"; t0: number; dir: 1 | -1; speed: number; amp: number; tail: number };
export type Line = { id: number; from: string; to: string; task: string; pts: Float32Array; cum: Float32Array; length: number; hubS: number; born: number; dies: number; pulses: Pulse[] };

export const LINE_POINTS = 16;
export const PULSES_PER_LINE = 6;
const FADE_IN = 0.6;
const FADE_OUT = 1.6;

function cubic(p0: Pt, p1: Pt, p2: Pt, p3: Pt, n: number, out: Pt[]) {
  for (let i = out.length ? 1 : 0; i <= n; i++) {
    const t = i / n, u = 1 - t;
    out.push([u * u * u * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t * t * t * p3[0], u * u * u * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t * t * t * p3[1]]);
  }
}

/** Agent A → hub → agent B: a near-straight spoke in, a smooth turn through the hub (heading A → B), a spoke out. */
export function linePath(l: WebLayout, a: WebAgent, b: WebAgent) {
  const h: Pt = [l.hub.cx, l.hub.cy];
  const dx = b.x - a.x, dy = b.y - a.y;
  const n = Math.hypot(dx, dy) || 1;
  const d: Pt = [dx / n, dy / n];
  const ka = Math.hypot(h[0] - a.x, h[1] - a.y) * 0.3;
  const kb = Math.hypot(b.x - h[0], b.y - h[1]) * 0.3;
  const pts: Pt[] = [];
  const half = LINE_POINTS / 2;
  cubic([a.x, a.y], [a.x + (h[0] - a.x) * 0.35, a.y + (h[1] - a.y) * 0.35], [h[0] - d[0] * ka, h[1] - d[1] * ka], h, half - 1, pts);
  cubic(h, [h[0] + d[0] * kb, h[1] + d[1] * kb], [b.x + (h[0] - b.x) * 0.35, b.y + (h[1] - b.y) * 0.35], [b.x, b.y], half, pts);
  const flat = new Float32Array(pts.length * 2);
  const cum = new Float32Array(pts.length);
  pts.forEach((p, i) => {
    flat.set(p, i * 2);
    if (i) cum[i] = cum[i - 1] + Math.hypot(p[0] - pts[i - 1][0], p[1] - pts[i - 1][1]);
  });
  return { pts: flat, cum, length: cum[cum.length - 1], hubS: cum[half - 1] };
}

/** A deterministic stream of lines (one task each), planned in time order. */
export function createWeb(l: WebLayout, seed = 11) {
  const rand = rng(seed);
  const orch = l.agents.filter((a) => a.role === "orchestrator");
  const work = l.agents.filter((a) => a.role === "worker");
  const lines: Line[] = [];
  let clock = 0.2;
  let id = 0;
  let last = "";
  /** Both ends' places must be fully on screen for the line's whole life. */
  const steady = (a: WebAgent, t0: number, t1: number) => placeSteady(a.slot, l.slots[a.slot], a.j, t0, t1);

  /** Try to start a line at `t`; false if no pair of places can carry one for its whole life. */
  function plan(t: number): boolean {
    const busy = new Set(lines.filter((x) => x.dies + FADE_OUT > t).flatMap((x) => [x.to]));
    const from = orch.filter((a) => steady(a, t, t + 2));
    const to = work.filter((w) => !busy.has(w.id) && steady(w, t, t + 2));
    if (!from.length || !to.length) return false;
    for (let attempt = 0; attempt < 4; attempt++) {
      let a = from[Math.floor(rand() * from.length)];
      const b = to[Math.floor(rand() * to.length)];
      if (`${a.id}>${b.id}` === last) a = from[(from.indexOf(a) + 1) % from.length];
      const line = build(t, a, b);
      if (steady(a, t, line.dies + FADE_OUT) && steady(b, t, line.dies + FADE_OUT)) {
        last = `${a.id}>${b.id}`;
        lines.push(line);
        return true;
      }
    }
    return false;
  }

  function build(t: number, a: WebAgent, b: WebAgent): Line {
    const path = linePath(l, a, b);
    const v = l.speed;
    const task = { kind: "task" as const, t0: t + FADE_IN * 0.6, dir: 1 as const, speed: 360 * v, amp: 0.95, tail: 70 };
    const pulses: Pulse[] = [task];
    let at = task.t0 + path.length / task.speed + 0.25;
    const ticks = 2 + Math.floor(rand() * 2);
    for (let i = 0; i < ticks; i++, at += 0.45 + rand() * 0.35) pulses.push({ kind: "progress", t0: at, dir: -1, speed: 480 * v, amp: 0.45, tail: 34 });
    const result: Pulse = { kind: "result", t0: at + 0.5 + rand() * 0.5, dir: -1, speed: 330 * v, amp: 1.3, tail: 120 };
    pulses.push(result);
    const dies = result.t0 + path.length / result.speed;
    const hex = Math.floor(rand() * 0xffffffff).toString(16).padStart(8, "0");
    return { id: id++, from: a.id, to: b.id, task: `task.${hex}`, ...path, born: t, dies, pulses };
  }

  function advance(t: number) {
    while (clock <= t) {
      // A miss (places mid-swap, everyone busy) retries shortly, so the web never sits empty for long.
      clock += plan(clock) ? l.every[0] + rand() * (l.every[1] - l.every[0]) : 0.4;
    }
  }

  /** Lines with any light at `t`, newest last, at most `maxLines`. */
  function live(t: number) {
    advance(t);
    for (let i = lines.length - 1; i >= 0; i--) if (lines[i].dies + FADE_OUT < t - 4) lines.splice(i, 1);
    return lines.filter((x) => x.born <= t && x.dies + FADE_OUT > t).slice(-l.maxLines);
  }

  return { live, advance, lines: () => lines as readonly Line[] };
}

const smooth = (x: number) => (x <= 0 ? 0 : x >= 1 ? 1 : x * x * (3 - 2 * x));
export const aliveAt = (x: Line, t: number) => smooth((t - x.born) / FADE_IN) * (1 - smooth((t - x.dies) / FADE_OUT));

/** Where a pulse's head is (arc length), or null if it has not started. */
export function headAt(x: Line, p: Pulse, t: number) {
  if (t < p.t0) return null;
  const run = (t - p.t0) * p.speed;
  return p.dir > 0 ? run : x.length - run;
}

/** Point at arc length `s` on a line. */
export function pointAt(x: Line, s: number): Pt {
  let i = 1;
  while (i < x.cum.length - 1 && x.cum[i] < s) i++;
  const u = (s - x.cum[i - 1]) / (x.cum[i] - x.cum[i - 1] || 1);
  return [x.pts[i * 2 - 2] + (x.pts[i * 2] - x.pts[i * 2 - 2]) * u, x.pts[i * 2 - 1] + (x.pts[i * 2 + 1] - x.pts[i * 2 - 1]) * u];
}

// ---- the frame state the shader reads (and the poster evaluates) ----

export const RIPPLES = 8;
const LAMP_EVENTS = 12;
const W_KIND = { task: 0.8, progress: 0.35, result: 1 } as const;

export type WebFrame = {
  lines: { alive: number; pts: Float32Array; pulses: [number, number, number, number][] }[];
  events: [number, number, number, number][];
  ripples: [number, number, number, number][];
  glow: number;
  /** Visitor clicks: [x, y, radius, amp], neutral rings (at most CLICKS). */
  clicks: [number, number, number, number][];
  /** The copy block the field yields around (design units): x0, y0, x1, y1, feather. */
  mask: [number, number, number, number, number];
};

/** The moment the web's poster shows and its live layer starts from. */
export const WEB_POSTER_T = 30;

export const CLICKS = 4;
/** Feather of the copy mask, design units (at least 120 CSS px down to a 760 px tall hero). */
export const FEATHER = 170;
const CLICK_LIFE = 1.5;

/** A visitor's click: one neutral ring out from the pointer, a little stronger than a hub ripple. */
export function clickRings(clicks: readonly { x: number; y: number; t: number }[], t: number): WebFrame["clicks"] {
  const out: WebFrame["clicks"] = clicks
    .filter((c) => t >= c.t && t - c.t < CLICK_LIFE)
    .slice(-CLICKS)
    .map((c) => [c.x, c.y, (t - c.t) * 520, Math.exp(-(t - c.t) / 0.5)]);
  while (out.length < CLICKS) out.push([0, 0, 0, 0]);
  return out;
}

/** Everything visible at `t`: line lift, pulse heads, hub rim flashes, ripples and the throughput glow. */
export function frameAt(l: WebLayout, lines: readonly Line[], t: number, extra: { clicks?: WebFrame["clicks"]; copy?: readonly number[] } = {}): WebFrame {
  const events: WebFrame["events"] = [];
  const ripples: WebFrame["ripples"] = [];
  let glow = 0;
  const out = lines.map((x) => {
    const pulses: [number, number, number, number][] = [];
    for (const p of x.pulses) {
      const s = headAt(x, p, t);
      if (s === null) continue;
      const signed = p.kind === "task" ? p.amp : -p.amp;
      if (s > -p.tail && s < x.length + p.tail) pulses.push([s, signed, p.dir, p.tail]);
      // Crossing the hub: flash where it enters the plate, ring out through the field.
      const age = t - (p.t0 + (p.dir > 0 ? x.hubS : x.length - x.hubS) / p.speed);
      if (age >= 0) {
        const w = W_KIND[p.kind];
        glow += w * Math.exp(-age / 2.5);
        if (age < 1.5) events.push([...pointAt(x, x.hubS - p.dir * l.plate.w * 0.5), age, p.kind === "task" ? w : -w]);
        if (age < 4) ripples.push([age * 260 * Math.max(l.speed, 0.6), w * Math.exp(-age / 1.6), 26 + age * 10, 0]);
      }
    }
    return { alive: aliveAt(x, t), pts: x.pts, pulses: pulses.slice(-PULSES_PER_LINE) };
  });
  events.sort((a, b) => a[2] - b[2]);
  ripples.sort((a, b) => b[1] - a[1]);
  events.length = Math.min(events.length, LAMP_EVENTS);
  ripples.length = Math.min(ripples.length, RIPPLES);
  while (events.length < LAMP_EVENTS) events.push([0, 0, 99, 0]);
  while (ripples.length < RIPPLES) ripples.push([0, 0, 1, 0]);
  const c = extra.copy ?? l.copy;
  const clicks = extra.clicks ?? clickRings([], t);
  return { lines: out, events, ripples, glow: 1 - Math.exp(-glow / 4), clicks, mask: [c[0], c[1], c[2], c[3], FEATHER] };
}
