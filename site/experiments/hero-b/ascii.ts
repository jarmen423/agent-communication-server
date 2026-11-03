// Concept B, ASCII variant: the same long exposure (light.ts, with light that
// crosses the junction), gathered per character cell (cells.wgsl, the one
// extra pass) and drawn as a still field of protocol text plus the hub lamp
// (ascii.wgsl, which replaces B's composite).
import { effect, init, sampler, surface, target, texture } from "vgpu";
import type { Frame, Gpu, Target, Texture } from "vgpu";
import type { HeroModule, HeroPalette } from "@/components/hero/gpu-stage";
import asciiShader from "./ascii.wgsl";
import cellsShader from "./cells.wgsl";
import { fieldRows, FONT, glyphTable, GRID, LOOK, RAMP, TUNE } from "./field";
import { layoutFor, type Layout } from "./layout";
import { createLight } from "./light";
import { run } from "./run";
import { ATLAS_COLS, canvasAtlas, monoFamily, rgba, type AtlasMaker } from "./atlas";
import { hubLamp } from "./schedule";




/** The gather pass on its own (the poster bake reads it back). */
export function createCells(gpu: Gpu, light: Target, grid: readonly [number, number], px: number) {
  const cells = target(gpu, { size: [grid[0], grid[1]], format: "rgba16float", label: "hero-b.cells" });
  const params = (size: readonly [number, number], k: number) => ({ cell: [size[0] / grid[0], size[1] / grid[1]], stride: Math.max(1, Math.floor(k)), gain: LOOK.gain });
  const gather = effect(gpu, cellsShader, { set: { light, gather: params(light.size, px) } });
  return {
    cells,
    compile: () => gather.compile(cells),
    encode: (f: Frame) => f.pass(cells, gather),
    resize: (size: readonly [number, number], k: number) => gather.set({ gather: params(size, k) }),
    dispose: () => gather.dispose(),
  };
}

export function createField(gpu: Gpu, layout: Layout, palette: HeroPalette, size: readonly [number, number], makeAtlas: AtlasMaker) {
  const grid = layout.compact ? GRID.tall : GRID.wide;
  const ex = createLight(gpu, layout, size, { through: true });
  const gather = createCells(gpu, ex.light, grid, size[0] / layout.width);
  const { chars, index } = glyphTable(fieldRows(layout, grid[0], grid[1]));
  // Four-byte texels (glyph index in R): tiny, universally supported, and what the mock adapter models.
  const text = texture(gpu, { kind: "2d", size: [grid[0], grid[1]], format: "rgba8uint", usage: ["texture_binding", "copy_dst"], label: "hero-b.text" });
  gpu.gpu.queue.writeTexture({ texture: text.gpu }, rgba(index), { bytesPerRow: grid[0] * 4 }, [grid[0], grid[1]]);

  const cellPx = (s: readonly [number, number]): [number, number] => [s[0] / grid[0], s[1] / grid[1]];
  let atlas: Texture;
  let slot: [number, number];
  const upload = (s: readonly [number, number]) => {
    const made = makeAtlas(chars, cellPx(s), cellPx(s)[1] * FONT.scale);
    atlas?.destroy();
    atlas = texture(gpu, { kind: "2d", size: [made.width, made.height], format: "rgba8unorm", usage: ["texture_binding", "copy_dst"], label: "hero-b.atlas" });
    gpu.gpu.queue.writeTexture({ texture: atlas.gpu }, rgba(made.data), { bytesPerRow: made.width * 4 }, [made.width, made.height]);
    slot = made.slot;
  };
  upload(size);

  const ramp = RAMP.chars.map((c) => chars.indexOf(c));
  const fieldParams = (s: readonly [number, number], p: HeroPalette) => ({
    grid: [grid[0], grid[1]],
    cell: cellPx(s),
    slot,
    atlas_cols: ATLAS_COLS,
    px: s[0] / layout.width,
    muted: [...p.laneMuted, 1],
    teal: [...p.teal, 1],
    orange: [...p.orange, 1],
    look: [LOOK.base, LOOK.scale, LOOK.power, 3],
    ramp_at: RAMP.at,
    ramp,
  });
  const { plate } = layout;
  const lampParams = (t: number) => {
    const l = hubLamp(ex.traffic.live(t), t);
    return { plate: [plate.x, plate.y, plate.w, plate.h], glow: [l.glow, l.share, plate.r, 0.6], events: l.events };
  };
  let pal = palette;
  let current = size;
  const draw = effect(gpu, asciiShader, {
    set: { cells: gather.cells, text, atlas: atlas!, samp: sampler(gpu, { minFilter: "linear", magFilter: "linear" }), field: fieldParams(size, pal), lamp: lampParams(ex.time) },
  });

  return {
    ...ex,
    compile: (out: Target) => Promise.all([ex.compile(), gather.compile(), draw.compile(out)]),
    encode(f: Frame, out: Target, dt: number) {
      ex.encode(f, dt);
      gather.encode(f);
      draw.set({ lamp: lampParams(ex.time) });
      f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => p.draw(draw));
    },
    resize(w: number, h: number) {
      current = [w, h];
      ex.resize(w, h);
      gather.resize(current, w / layout.width);
      upload(current);
      draw.set({ atlas: atlas!, field: fieldParams(current, pal) });
    },
    setPalette(p: HeroPalette) {
      pal = p;
      draw.set({ field: fieldParams(current, pal) });
    },
    dispose() {
      ex.dispose();
      gather.dispose();
      draw.dispose();
      text.destroy();
      atlas?.destroy();
    },
  };
}


export const startHero: HeroModule["startHero"] = async (canvas, { scene, palette }) => {
  const family = await monoFamily(canvas);
  const gpu = await init();
  try {
    const out = surface(gpu, canvas, { dpr: [1, 2] });
    const layout = layoutFor(scene, scene.compact ? TUNE.tall : TUNE.wide);
    const fx = createField(gpu, layout, palette, out.size, canvasAtlas(family));
    out.onResize(({ width, height }) => fx.resize(width, height));
    await fx.compile(out);
    return await run(gpu, canvas, { encode: (f, dt) => fx.encode(f, out, dt), setPalette: fx.setPalette, dispose: fx.dispose });
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
