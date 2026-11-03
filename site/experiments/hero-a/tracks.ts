// Track geometry for the signal box: every route's two tracks as flat
// polylines with arc length, matching roundedPath() in the shared geometry, so
// the SVG poster and the GPU layer put lamps, tiles and pulses on the same pixels.
import { offset, type Pt, type Scene } from "@/components/hero/geometry";

export type Dir = "sent" | "received";

export type Track = {
  /** Index in buildTracks(): route index × 2, + 1 for the received track. */
  k: number;
  route: string;
  dir: Dir;
  pts: Pt[];
  /** Cumulative arc length at each point; cum[0] = 0. */
  cum: number[];
  length: number;
};

const sub = (a: Pt, b: Pt): Pt => [a[0] - b[0], a[1] - b[1]];
const len = (v: Pt) => Math.hypot(v[0], v[1]);
const norm = (v: Pt): Pt => {
  const l = len(v) || 1;
  return [v[0] / l, v[1] / l];
};

/** roundedPath() as points: each bend is the same quadratic, sampled in `steps` chords. */
export function roundedPolyline(points: Pt[], r: number, steps = 6): Pt[] {
  const out: Pt[] = [points[0]];
  for (let i = 1; i < points.length - 1; i++) {
    const p = points[i];
    const a = norm(sub(points[i - 1], p));
    const b = norm(sub(points[i + 1], p));
    const ra = Math.min(r, len(sub(points[i - 1], p)) / 2);
    const rb = Math.min(r, len(sub(points[i + 1], p)) / 2);
    const p0: Pt = [p[0] + a[0] * ra, p[1] + a[1] * ra];
    const p2: Pt = [p[0] + b[0] * rb, p[1] + b[1] * rb];
    for (let j = 0; j <= steps; j++) {
      const t = j / steps;
      const u = 1 - t;
      out.push([u * u * p0[0] + 2 * u * t * p[0] + t * t * p2[0], u * u * p0[1] + 2 * u * t * p[1] + t * t * p2[1]]);
    }
  }
  out.push(points[points.length - 1]);
  return out;
}

function track(k: number, route: string, dir: Dir, pts: Pt[]): Track {
  const cum = [0];
  for (let i = 1; i < pts.length; i++) cum.push(cum[i - 1] + len(sub(pts[i], pts[i - 1])));
  return { k, route, dir, pts, cum, length: cum[cum.length - 1] };
}

/** Two tracks per route, sent (left of travel) then received, in scene.routes order. */
export function buildTracks(scene: Scene): Track[] {
  return scene.routes.flatMap((r, i) => [
    track(i * 2, r.id, "sent", roundedPolyline(offset(r.points, scene.gap), scene.bend)),
    track(i * 2 + 1, r.id, "received", roundedPolyline(offset(r.points, -scene.gap), scene.bend)),
  ]);
}

/** Point at arc length `s`; beyond either end it continues straight, so tiles can slide under a card. */
export function at(t: Track, s: number): Pt {
  const { pts, cum } = t;
  let i = 1;
  while (i < pts.length - 1 && cum[i] < s) i++;
  const a = pts[i - 1];
  const b = pts[i];
  const seg = cum[i] - cum[i - 1] || 1;
  const u = (s - cum[i - 1]) / seg;
  return [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
}

/** SVG path for the part of a track between arc lengths lo and hi. */
export function subPath(t: Track, lo: number, hi: number): string {
  const f = (n: number) => Math.round(n * 100) / 100;
  const pts: Pt[] = [at(t, lo)];
  t.cum.forEach((c, i) => {
    if (c > lo && c < hi) pts.push(t.pts[i]);
  });
  pts.push(at(t, hi));
  return pts.map((p, i) => `${i ? "L" : "M"}${f(p[0])} ${f(p[1])}`).join("");
}
