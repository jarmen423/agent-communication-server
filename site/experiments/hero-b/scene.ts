// Concept B GPU layer: the long exposure (light.ts), with light crossing the
// junction behind the glass hub plate, developed by composite.wgsl into soft
// brand-coloured light over the lane band, with the hub lamp underneath.
import { effect, init, surface } from "vgpu";
import type { Frame, Gpu, Target } from "vgpu";
import type { HeroModule, HeroPalette } from "@/components/hero/gpu-stage";
import compositeShader from "./composite.wgsl";
import { layoutFor, type Layout } from "./layout";
import { createLight } from "./light";
import { run } from "./run";
import { hubLamp } from "./schedule";

/** Soft saturation of the light; a single fresh trail sits near half of its colour. */
const GAIN = 0.7;

type Palette = Pick<HeroPalette, "teal" | "orange"> & Partial<Pick<HeroPalette, "laneInk">>;
const look = (p: Palette) => ({ teal: [...p.teal, 1], orange: [...p.orange, 1], gain: GAIN, opacity: 1 });

/** `lamp: false` bakes trails only (the poster draws the neutral glow in SVG; a teal/orange PNG palette can't). */
export function createExposure(gpu: Gpu, layout: Layout, palette: Palette, size: readonly [number, number], opts: { lamp?: boolean } = {}) {
  const ex = createLight(gpu, layout, size, { through: true });
  const { plate } = layout;
  let ink = palette.laneInk ?? [0.933, 0.941, 0.918];
  let px = size[0] / layout.width;
  const lampUniform = () => {
    const l = hubLamp(ex.traffic.live(ex.time), ex.time);
    return { plate: [plate.x, plate.y, plate.w, plate.h], glow: [l.glow, plate.r, opts.lamp === false ? 0 : 0.75, px], ink: [...ink, 1], events: l.events, ripples: l.ripples };
  };
  const develop = effect(gpu, compositeShader, { set: { light: ex.light, look: look(palette), lamp: lampUniform() } });
  return {
    ...ex,
    get time() {
      return ex.time;
    },
    get count() {
      return ex.count;
    },
    compile: (out: Target) => Promise.all([ex.compile(), develop.compile(out)]),
    encode(f: Frame, out: Target, dt: number) {
      ex.encode(f, dt);
      develop.set({ lamp: lampUniform() });
      f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => p.draw(develop));
    },
    resize(w: number, h: number) {
      ex.resize(w, h);
      px = w / layout.width;
    },
    setPalette: (p: Palette) => {
      ink = p.laneInk ?? ink;
      develop.set({ look: look(p) });
    },
    dispose() {
      ex.dispose();
      develop.dispose();
    },
  };
}

export const startHero: HeroModule["startHero"] = async (canvas, { scene, palette }) => {
  const gpu = await init();
  try {
    const out = surface(gpu, canvas, { dpr: [1, 2] });
    const ex = createExposure(gpu, layoutFor(scene), palette, out.size);
    out.onResize(({ width, height }) => ex.resize(width, height));
    await ex.compile(out);
    return await run(gpu, canvas, { encode: (f, dt) => ex.encode(f, out, dt), setPalette: ex.setPalette, dispose: ex.dispose });
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
