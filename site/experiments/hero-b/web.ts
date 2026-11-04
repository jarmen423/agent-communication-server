// The ASCII web, live. Two passes per frame: web-cells.wgsl (analytic light,
// one texel per character cell) and web.wgsl (the field and the hub lamp),
// both cover-mapped from the design space onto the full-bleed hero canvas.
import { effect, init, sampler, surface, target, texture } from "vgpu";
import type { Frame, Gpu, Target, Texture } from "vgpu";
import type { HeroPalette, HeroRenderer } from "@/components/hero/gpu-stage";
import { ATLAS_COLS, canvasAtlas, monoFamily, rgba, type AtlasMaker } from "./atlas";
import { FONT, glyphTable } from "./glyphs";
import { run } from "./run";
import cellsShader from "./web-cells.wgsl";
import { WEB_LOOK, WEB_RAMP, YIELD, lineBox, lineSigma, webRows } from "./web-light";
import { clickRings, cover, createWeb, frameAt, LINE_POINTS, PULSES_PER_LINE, RIPPLES, WEB, WEB_POSTER_T, type Band, type WebFrame, type WebLayout } from "./web-model";
import fieldShader from "./web.wgsl";
import { placeAlpha } from "./web-places";

export type WebRenderer = HeroRenderer & {
  /** The copy block's rect in design units: the field yields around it. */
  setCopy(rect: readonly number[]): void;
  /** A visitor's click at a design-space point: one neutral ring. */
  click(x: number, y: number): void;
};
export type WebModule = { startWeb(canvas: HTMLCanvasElement, opts: { band: Band; palette: HeroPalette }): Promise<WebRenderer> };

/** The per-frame uniform for web-cells.wgsl. */
export function webUniform(l: WebLayout, f: WebFrame) {
  const [cols, rows] = l.grid;
  const lineInfo: number[][] = [];
  const boxes: number[][] = [];
  const pts: number[][] = [];
  const pulses: number[][] = [];
  for (let i = 0; i < 6; i++) {
    const line = f.lines[i];
    lineInfo.push(line ? [line.alive, line.pulses.length, 0, 0] : [0, 0, 0, 0]);
    boxes.push(line ? lineBox(line.pts, l) : [1, 1, -1, -1]);
    for (let j = 0; j < LINE_POINTS; j += 2) pts.push(line ? Array.from(line.pts.subarray(j * 2, j * 2 + 4)) : [0, 0, 0, 0]);
    for (let k = 0; k < PULSES_PER_LINE; k++) pulses.push(line?.pulses[k] ?? [0, 0, 1, 1]);
  }
  return {
    grid: [cols, rows],
    cell: [l.width / cols, l.height / rows],
    hub: [l.hub.cx, l.hub.cy],
    sigma: lineSigma(l),
    lines: Math.min(6, f.lines.length),
    look: [WEB_LOOK.ahead, WEB_LOOK.lift, WEB_LOOK.ripple, 0],
    line_info: lineInfo,
    line_box: boxes,
    pts,
    pulses,
    ripples: f.ripples.slice(0, RIPPLES),
    clicks: f.clicks,
    mask: f.mask.slice(0, 4),
    mask2: [f.mask[4], YIELD.floor, YIELD.radius, WEB_LOOK.click],
  };
}

export function createWebLayer(gpu: Gpu, l: WebLayout, palette: HeroPalette, size: readonly [number, number], makeAtlas: AtlasMaker) {
  const [cols, rows] = l.grid;
  const web = createWeb(l);
  let t = WEB_POSTER_T;
  let copy: readonly number[] = l.copy;
  const clicks: { x: number; y: number; t: number }[] = [];
  const frameNow = () => frameAt(l, web.live(t), t, { copy, clicks: clickRings(clicks, t) });
  const cells = target(gpu, { size: [cols, rows], format: "rgba16float", label: "hero-b.web-cells" });
  const light = effect(gpu, cellsShader, { set: { web: webUniform(l, frameNow()) } });
  const { chars, index } = glyphTable(webRows(cols, rows), WEB_RAMP.chars);
  const text = texture(gpu, { kind: "2d", size: [cols, rows], format: "rgba8uint", usage: ["texture_binding", "copy_dst"], label: "hero-b.web-text" });
  gpu.gpu.queue.writeTexture({ texture: text.gpu }, rgba(index), { bytesPerRow: cols * 4 }, [cols, rows]);

  let atlas: Texture | undefined;
  let slot: [number, number] = [1, 1];
  let current = size;
  let pal = palette;
  const upload = () => {
    const { k } = cover(l, current[0], current[1]);
    const cell: [number, number] = [(l.width / cols) * k, (l.height / rows) * k];
    const made = makeAtlas(chars, cell, cell[1] * FONT.scale);
    atlas?.destroy();
    atlas = texture(gpu, { kind: "2d", size: [made.width, made.height], format: "rgba8unorm", usage: ["texture_binding", "copy_dst"], label: "hero-b.web-atlas" });
    gpu.gpu.queue.writeTexture({ texture: atlas.gpu }, rgba(made.data), { bytesPerRow: made.width * 4 }, [made.width, made.height]);
    slot = made.slot;
  };
  upload();
  const fieldUniform = () => {
    const m = cover(l, current[0], current[1]);
    return {
      grid: [cols, rows],
      cell: [l.width / cols, l.height / rows],
      slot,
      atlas_cols: ATLAS_COLS,
      unused: 0,
      map: [m.k, m.x, m.y, 0],
      muted: [...pal.laneMuted, 1],
      ink: [...pal.laneInk, 1],
      teal: [...pal.teal, 1],
      orange: [...pal.orange, 1],
      look: [WEB_LOOK.base, WEB_LOOK.liftAlpha, WEB_LOOK.gain, WEB_LOOK.litScale],
      look2: [WEB_LOOK.hue, WEB_LOOK.ink, 0, 0],
      ramp_at: WEB_RAMP.at,
      ramp: WEB_RAMP.chars.map((c) => chars.indexOf(c)),
    };
  };
  const { plate } = l;
  const lampUniform = (f: WebFrame) => ({ plate: [plate.x, plate.y, plate.w, plate.h], glow: [f.glow, plate.r, 0.7, 0], events: f.events });
  const first = frameNow();
  const draw = effect(gpu, fieldShader, {
    set: { cells, text, atlas: atlas!, samp: sampler(gpu, { minFilter: "linear", magFilter: "linear" }), field: fieldUniform(), lamp: lampUniform(first) },
  });

  return {
    web,
    get time() {
      return t;
    },
    compile: (out: Target) => Promise.all([light.compile(cells), draw.compile(out)]),
    encode(f: Frame, out: Target, dt: number) {
      t += dt;
      while (clicks.length && t - clicks[0].t > 2) clicks.shift();
      const frame = frameNow();
      light.set({ web: webUniform(l, frame) });
      draw.set({ lamp: lampUniform(frame) });
      f.pass(cells, light);
      f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => p.draw(draw));
    },
    setCopy(rect: readonly number[]) {
      copy = rect;
    },
    click(x: number, y: number) {
      clicks.push({ x, y, t });
      if (clicks.length > 4) clicks.shift();
    },
    resize(w: number, h: number) {
      current = [w, h];
      upload();
      draw.set({ atlas: atlas!, field: fieldUniform() });
    },
    setPalette(p: HeroPalette) {
      pal = p;
      draw.set({ field: fieldUniform() });
    },
    dispose() {
      light.dispose();
      draw.dispose();
      text.destroy();
      atlas?.destroy();
    },
  };
}

/** About 3.2 M device px: 1.5 on a 1440 × 900 retina hero, ~1.13 on 2560 × 980, never below 1. */
export const PIXEL_BUDGET = 3.2e6;
export const dprCap = (w: number, h: number) => Math.max(1, Math.min(1.5, Math.sqrt(PIXEL_BUDGET / Math.max(1, w * h))));

export const startWeb: WebModule["startWeb"] = async (canvas, { band, palette }) => {
  const layout = WEB[band];
  const family = await monoFamily(canvas);
  const gpu = await init();
  try {
    // A full-viewport canvas: at most 1.5 device px per CSS px, and less on very
    // large heroes so the field pass stays near a fixed pixel budget.
    const out = surface(gpu, canvas, { dpr: [1, dprCap(canvas.clientWidth, canvas.clientHeight)] });
    const layer = createWebLayer(gpu, layout, palette, out.size, canvasAtlas(family));
    out.onResize(({ width, height }) => layer.resize(width, height));
    await layer.compile(out);
    // Places come and go: fade their cards in the overlay (DOM, a handful of elements) with the same schedule.
    const cards = [...(canvas.closest?.("section")?.querySelectorAll<SVGGElement>(`[data-band="${band}"] [data-place]`) ?? [])].map((el) => {
      const [i, j] = (el.dataset.place ?? "0:0").split(":").map(Number);
      return { el, i, j, alpha: -1 };
    });
    const fadeCards = () => {
      for (const c of cards) {
        const a = Math.round(placeAlpha(c.i, layout.slots[c.i], c.j, layer.time) * 100) / 100;
        if (a !== c.alpha) c.el.setAttribute("opacity", String((c.alpha = a)));
      }
    };
    const renderer = await run(gpu, canvas, {
      encode: (f, dt) => {
        layer.encode(f, out, dt);
        fadeCards();
      },
      setPalette: layer.setPalette,
      dispose: layer.dispose,
    });
    return { ...renderer, setCopy: layer.setCopy, click: layer.click };
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
