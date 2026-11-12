// The ASCII web, live. Per frame: plain B's long exposure on the web's
// tracks (web-trails.ts: fade + deposits into a light target), the ground's
// cells (web-cells.wgsl: hub and click rings, one texel per character), then
// one pass to the canvas: the field and the hub lamp (web.wgsl) and the
// developed tracks over them (web-trails.wgsl). All cover-mapped from the
// design space onto the full-bleed hero canvas.
import { effect, init, sampler, surface, target, texture } from "vgpu";
import type { Frame, Gpu, Target, Texture } from "vgpu";
import type { HeroPalette, HeroRenderer } from "@/components/hero/gpu-stage";
import { ATLAS_COLS, canvasAtlas, monoFamily, rgba, type AtlasMaker } from "./atlas";
import { FONT, glyphTable } from "./glyphs";
import { run } from "./run";
import cellsShader from "./web-cells.wgsl";
import { CLICK_YIELD, WEB_LOOK, YIELD, webRows } from "./web-light";
import { clickRings, cover, FEATHER, groundFrame, TORCH, WEB, type Band, type WebFrame, type WebLayout } from "./web-model";
import { createTrails } from "./web-trails";
import fieldShader from "./web.wgsl";
import { placeAlpha } from "./web-places";

export type WebRenderer = HeroRenderer & {
  /** The copy block's rect in design units, then the headline's bottom: the field yields around it. */
  setCopy(rect: readonly number[]): void;
  /** A visitor's click at a design-space point: one neutral ring. */
  click(x: number, y: number): void;
  /** The mouse is over the field at a design-space point: the torch eases after it. */
  hover(x: number, y: number): void;
  /** The mouse left the hero: the torch fades out. */
  leave(): void;
};
export type WebModule = { startWeb(canvas: HTMLCanvasElement, opts: { band: Band; palette: HeroPalette }): Promise<WebRenderer> };

/** The per-frame uniform for web-cells.wgsl. */
export function webUniform(l: WebLayout, f: WebFrame) {
  return {
    hub: [l.hub.cx, l.hub.cy],
    cell: [l.width / l.grid[0], l.height / l.grid[1]],
    look: [WEB_LOOK.ripple, WEB_LOOK.click, CLICK_YIELD.floor, CLICK_YIELD.feather],
    ripples: f.ripples,
    clicks: f.clicks,
    mask: f.mask.slice(0, 4),
    mask2: [FEATHER, YIELD.floor, YIELD.radius, WEB_LOOK.torch],
    torch: f.torch,
  };
}

/** The light target's size for a canvas: CSS resolution (trails are soft light; the develop pass upsamples). */
const lightSize = (w: number, h: number, css: number): [number, number] => {
  const k = css > 0 ? Math.min(1, Math.max(0.5, css / w)) : 1;
  return [Math.max(1, Math.round(w * k)), Math.max(1, Math.round(h * k))];
};

export function createWebLayer(gpu: Gpu, l: WebLayout, palette: HeroPalette, size: readonly [number, number], makeAtlas: AtlasMaker, cssWidth = 0) {
  const [cols, rows] = l.grid;
  const trails = createTrails(gpu, l, lightSize(size[0], size[1], cssWidth), { cover: true, palette });
  trails.setOutput(size);
  let copy: readonly number[] = l.copy;
  const clicks: { x: number; y: number; t: number }[] = [];
  const now = () => trails.ex.time;
  // The torch: eased position and amp, toward the pointer's target (on = over the field).
  const torch = { x: 0, y: 0, amp: 0, tx: 0, ty: 0, on: false };
  const torchFrame = (): WebFrame["torch"] => [torch.x, torch.y, TORCH.radius, torch.amp];
  const frameNow = () => groundFrame(l, trails.traffic.live(now()), now(), { copy, clicks: clickRings(clicks, now()), torch: torchFrame() });
  const cells = target(gpu, { size: [cols, rows], format: "rgba16float", label: "hero-b.web-cells" });
  const light = effect(gpu, cellsShader, { set: { web: webUniform(l, frameNow()) } });
  const { chars, index } = glyphTable(webRows(cols, rows), []);
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
      look: [WEB_LOOK.base, WEB_LOOK.liftAlpha, WEB_LOOK.ink, 0],
    };
  };
  const { plate } = l;
  const lampUniform = (f: WebFrame) => ({ plate: [plate.x, plate.y, plate.w, plate.h], glow: [f.glow, plate.r, 0.7, 0], events: f.events });
  const draw = effect(gpu, fieldShader, {
    set: { cells, text, atlas: atlas!, samp: sampler(gpu, { minFilter: "linear", magFilter: "linear" }), field: fieldUniform(), lamp: lampUniform(frameNow()) },
  });

  return {
    traffic: trails.traffic,
    get time() {
      return now();
    },
    compile: (out: Target) => Promise.all([light.compile(cells), draw.compile(out), trails.compile(out)]),
    encode(f: Frame, out: Target, dt: number) {
      trails.ex.encode(f, dt);
      while (clicks.length && now() - clicks[0].t > 2) clicks.shift();
      const kf = 1 - Math.exp(-dt * TORCH.follow);
      torch.x += (torch.tx - torch.x) * kf;
      torch.y += (torch.ty - torch.y) * kf;
      torch.amp += ((torch.on ? 1 : 0) - torch.amp) * (1 - Math.exp(-dt * TORCH.fade));
      const frame = frameNow();
      light.set({ web: webUniform(l, frame) });
      draw.set({ lamp: lampUniform(frame) });
      f.pass(cells, light);
      f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => {
        p.draw(draw);
        p.draw(trails.develop);
      });
    },
    setCopy(rect: readonly number[]) {
      copy = rect;
      trails.setCopy(rect);
    },
    click(x: number, y: number) {
      clicks.push({ x, y, t: now() });
      if (clicks.length > 4) clicks.shift();
    },
    hover(x: number, y: number) {
      // Fresh in from outside: start under the pointer rather than sliding in from the last spot.
      if (torch.amp < 0.02) {
        torch.x = x;
        torch.y = y;
      }
      torch.tx = x;
      torch.ty = y;
      torch.on = true;
    },
    leave() {
      torch.on = false;
    },
    resize(w: number, h: number, css = 0) {
      current = [w, h];
      upload();
      draw.set({ atlas: atlas!, field: fieldUniform() });
      trails.resize([w, h], lightSize(w, h, css));
    },
    setPalette(p: HeroPalette) {
      pal = p;
      draw.set({ field: fieldUniform() });
      trails.setPalette(p);
    },
    dispose() {
      light.dispose();
      draw.dispose();
      trails.dispose();
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
    const layer = createWebLayer(gpu, layout, palette, out.size, canvasAtlas(family), canvas.clientWidth);
    out.onResize(({ width, height }) => layer.resize(width, height, canvas.clientWidth));
    await layer.compile(out);
    // Places come and go: fade their cards and rails in the overlay (DOM, a few dozen elements) with the same schedule.
    const cards = [...(canvas.closest?.("section")?.querySelectorAll<SVGGElement>(`[data-band="${band}"] :is([data-place], [data-rail])`) ?? [])].map((el) => {
      const [i, j] = (el.dataset.place ?? el.dataset.rail ?? "0:0").split(":").map(Number);
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
    return { ...renderer, setCopy: layer.setCopy, click: layer.click, hover: layer.hover, leave: layer.leave };
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
