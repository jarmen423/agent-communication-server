// Shared geometry for the hero's route diagram. The static poster (SVG) draws
// from it today; the GPU layer reads the same coordinates, so the two line up.

export type Pt = readonly [number, number];

export type DiagramNode = {
  id: string;
  product: string;
  machine: string;
  role: "orchestrator" | "worker";
  state: string;
  x: number;
  y: number;
  w: number;
  h: number;
};

/** A route is drawn in the direction a task travels (orchestrator → hub → worker). */
export type Route = { id: string; node: string; points: Pt[] };

export type Scene = {
  width: number;
  height: number;
  hub: { cx: number; cy: number; size: number };
  nodes: DiagramNode[];
  routes: Route[];
  /** Distance from a route's centre line to each of its two tracks. */
  gap: number;
  /** Corner radius where a route bends. */
  bend: number;
  compact: boolean;
};

const ORCH = { id: "me", product: "Claude Code", machine: "laptop", role: "orchestrator", state: "waiting on 2 tasks" } as const;
const WORKERS = [
  { id: "claude-1", product: "Claude Code", machine: "workstation", role: "worker", state: "working" },
  { id: "codex-1", product: "Codex", machine: "build-box", role: "worker", state: "task arriving" },
  { id: "hermes-1", product: "Hermes", machine: "home-server", role: "worker", state: "idle" },
] as const;

/** Desktop: left to right. viewBox 1200 × 440. */
export const WIDE: Scene = {
  width: 1200,
  height: 440,
  gap: 7,
  bend: 22,
  compact: false,
  hub: { cx: 600, cy: 220, size: 72 },
  nodes: [
    { ...ORCH, x: 32, y: 172, w: 224, h: 96 },
    { ...WORKERS[0], x: 944, y: 40, w: 224, h: 96 },
    { ...WORKERS[1], x: 944, y: 172, w: 224, h: 96 },
    { ...WORKERS[2], x: 944, y: 304, w: 224, h: 96 },
  ],
  routes: [
    { id: "a", node: "me", points: [[256, 220], [556, 220]] },
    { id: "b1", node: "claude-1", points: [[644, 220], [704, 220], [796, 88], [944, 88]] },
    { id: "b2", node: "codex-1", points: [[644, 220], [944, 220]] },
    { id: "b3", node: "hermes-1", points: [[644, 220], [704, 220], [796, 352], [944, 352]] },
  ],
};

/** Phones: top to bottom. viewBox 360 × 440. */
export const TALL: Scene = {
  width: 360,
  height: 440,
  gap: 5,
  bend: 14,
  compact: true,
  hub: { cx: 180, cy: 226, size: 60 },
  nodes: [
    { ...ORCH, x: 70, y: 12, w: 220, h: 64 },
    { ...WORKERS[0], x: 8, y: 356, w: 108, h: 72 },
    { ...WORKERS[1], x: 126, y: 356, w: 108, h: 72 },
    { ...WORKERS[2], x: 244, y: 356, w: 108, h: 72 },
  ],
  routes: [
    { id: "a", node: "me", points: [[180, 76], [180, 190]] },
    { id: "b1", node: "claude-1", points: [[180, 262], [180, 294], [62, 308], [62, 356]] },
    { id: "b2", node: "codex-1", points: [[180, 262], [180, 356]] },
    { id: "b3", node: "hermes-1", points: [[180, 262], [180, 294], [298, 308], [298, 356]] },
  ],
};

// ---- polyline helpers ----

const sub = (a: Pt, b: Pt): Pt => [a[0] - b[0], a[1] - b[1]];
const len = (v: Pt) => Math.hypot(v[0], v[1]);
const norm = (v: Pt): Pt => {
  const l = len(v) || 1;
  return [v[0] / l, v[1] / l];
};
/** Left of travel, in screen coordinates (y grows downward). */
const left = (v: Pt): Pt => [v[1], -v[0]];

/** Parallel copy of a polyline, `d` to the left of travel (negative = right), mitred at bends. */
export function offset(points: Pt[], d: number): Pt[] {
  return points.map((p, i) => {
    const prev = i > 0 ? norm(sub(p, points[i - 1])) : null;
    const next = i < points.length - 1 ? norm(sub(points[i + 1], p)) : null;
    const nPrev = prev ? left(prev) : null;
    const nNext = next ? left(next) : null;
    if (!nPrev) return [p[0] + nNext![0] * d, p[1] + nNext![1] * d] as Pt;
    if (!nNext) return [p[0] + nPrev[0] * d, p[1] + nPrev[1] * d] as Pt;
    const m = norm([nPrev[0] + nNext[0], nPrev[1] + nNext[1]]);
    const k = d / (m[0] * nNext[0] + m[1] * nNext[1]);
    return [p[0] + m[0] * k, p[1] + m[1] * k] as Pt;
  });
}

/** SVG path through the points with each bend rounded by radius `r`. */
export function roundedPath(points: Pt[], r: number): string {
  const f = (n: number) => Math.round(n * 100) / 100;
  let d = `M${f(points[0][0])} ${f(points[0][1])}`;
  for (let i = 1; i < points.length - 1; i++) {
    const p = points[i];
    const a = norm(sub(points[i - 1], p));
    const b = norm(sub(points[i + 1], p));
    const ra = Math.min(r, len(sub(points[i - 1], p)) / 2);
    const rb = Math.min(r, len(sub(points[i + 1], p)) / 2);
    d += `L${f(p[0] + a[0] * ra)} ${f(p[1] + a[1] * ra)}Q${f(p[0])} ${f(p[1])} ${f(p[0] + b[0] * rb)} ${f(p[1] + b[1] * rb)}`;
  }
  const last = points[points.length - 1];
  return `${d}L${f(last[0])} ${f(last[1])}`;
}

/** Point at fraction `t` (0..1) of a polyline's length. */
export function pointAt(points: Pt[], t: number): Pt {
  const segs = points.slice(1).map((p, i) => len(sub(p, points[i])));
  let target = Math.max(0, Math.min(1, t)) * segs.reduce((s, l) => s + l, 0);
  for (let i = 0; i < segs.length; i++) {
    if (target <= segs[i]) {
      const u = segs[i] ? target / segs[i] : 0;
      const a = points[i];
      const b = points[i + 1];
      return [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
    }
    target -= segs[i];
  }
  return points[points.length - 1];
}

/** The tracks of a route: sent runs on the left of travel, received on the right. */
export function tracks(scene: Scene, route: Route) {
  return { sent: offset(route.points, scene.gap), received: offset(route.points, -scene.gap) };
}

// ---- brand shapes ----

/** A message tile: the bubble with one sharp corner (sent: bottom-left, received: bottom-right). */
export function tilePath(cx: number, cy: number, w: number, h: number, dir: "sent" | "received", r = 4): string {
  const x = cx - w / 2;
  const y = cy - h / 2;
  return dir === "sent"
    ? `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h - r}A${r} ${r} 0 0 1 ${x + w - r} ${y + h}H${x}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`
    : `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h}H${x + r}A${r} ${r} 0 0 1 ${x} ${y + h - r}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`;
}

/**
 * The lens of the mark (where the two bubbles overlap: the server), on the
 * mark's 64-unit grid it spans 26..38. Returned scaled to `size` around a centre.
 */
export function lensPath(cx: number, cy: number, size: number): string {
  const s = size / 12;
  const X = (u: number) => Math.round((cx + (u - 32) * s) * 100) / 100;
  const Y = (u: number) => Math.round((cy + (u - 32) * s) * 100) / 100;
  const r = Math.round(10 * s * 100) / 100;
  return `M${X(36)} ${Y(26)}H${X(38)}V${Y(28)}A${r} ${r} 0 0 1 ${X(28)} ${Y(38)}H${X(26)}V${Y(36)}A${r} ${r} 0 0 1 ${X(36)} ${Y(26)}Z`;
}
