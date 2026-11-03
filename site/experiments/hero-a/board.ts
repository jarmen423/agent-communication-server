// Packs the signal box into the shader's two uniforms: `board` (geometry and
// palette, set once and on resize or theme change) and `live` (one frame of the
// script). Pure, so the Node test packs exactly what the browser uploads.
import type { HeroPalette } from "@/components/hero/gpu-stage";
import type { FrameState, Model } from "./script";

type V4 = [number, number, number, number];
const SEGS = 64;
const SPRITES = 8;

/** Sizes per stage, in scene units; they match the poster's strokes and tiles. */
export function sizes(compact: boolean) {
  return compact
    ? { lamp: 2, sigma: 3.4, pitch: 16, gap: 2.4, tile: [22, 16, 3] as const, pulse: 2.6, plateR: 14, cardR: 8 }
    : { lamp: 2.5, sigma: 4.4, pitch: 24, gap: 3, tile: [30, 22, 4] as const, pulse: 3.4, plateR: 20, cardR: 10 };
}

export function boardValues(m: Model, palette: HeroPalette, pxPerUnit: number) {
  const { scene, tracks } = m;
  const z = sizes(scene.compact);
  const reach = z.sigma * 3 + z.lamp;
  const segs: V4[] = [];
  const info: V4[] = [];
  for (const t of tracks) {
    const first = segs.length / 2;
    for (let i = 1; i < t.pts.length; i++) {
      segs.push([t.pts[i - 1][0], t.pts[i - 1][1], t.pts[i][0], t.pts[i][1]], [t.cum[i - 1], t.cum[i] - t.cum[i - 1], 0, 0]);
    }
    const xs = t.pts.map((p) => p[0]);
    const ys = t.pts.map((p) => p[1]);
    info.push(
      [first, t.pts.length - 1, t.length, t.dir === "received" ? 1 : 0],
      [Math.min(...xs) - reach, Math.min(...ys) - reach, Math.max(...xs) + reach, Math.max(...ys) + reach],
    );
  }
  if (segs.length > SEGS * 2 || info.length !== 16) throw new Error("hero-a: board exceeds the shader's arrays");
  while (segs.length < SEGS * 2) segs.push([0, 0, 0, 0]);
  const { hub } = scene;
  return {
    view: [scene.width, scene.height, pxPerUnit, 1 / pxPerUnit] as V4,
    dims: [z.lamp, z.sigma, z.pitch, z.gap] as V4,
    sprite: [z.tile[0], z.tile[1], z.tile[2], z.pulse] as V4,
    plate: [hub.cx, hub.cy, hub.size * 0.78, z.plateR] as V4,
    lens: [hub.size / 12, z.cardR, 0, 0] as V4,
    ...paint(palette),
    cards: scene.nodes.map((n) => [n.x, n.y, n.w, n.h] as V4),
    tracks: info,
    segs,
  };
}

export const paint = (p: HeroPalette) => ({ teal: [...p.teal, 1] as V4, orange: [...p.orange, 1] as V4, lane: [...p.lane, 1] as V4 });

export function liveValues(f: FrameState) {
  if (f.sprites.length > SPRITES) throw new Error(`hero-a: ${f.sprites.length} sprites, the shader draws ${SPRITES}`);
  const sprites: V4[] = [];
  for (let j = 0; j < SPRITES; j++) {
    const s = f.sprites[j];
    if (!s) sprites.push([0, 0, 0, 0], [0, 0, 0, 0]);
    else sprites.push([s.x, s.y, s.kind === "tile" ? 1 : 2, s.dir === "received" ? 1 : 0], [s.track, s.s, 0, 0]);
  }
  return {
    lit: f.lit.map(([lo, hi]) => [lo, hi, 0, 0] as V4),
    sprites,
    hub: [f.hub.sent, f.hub.received, 0, 0] as V4,
  };
}
