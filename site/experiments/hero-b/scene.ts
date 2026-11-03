// Concept B GPU layer: the long exposure (light.ts) developed by
// composite.wgsl into soft brand-coloured light over the lane band.
import { effect, init, surface } from "vgpu";
import type { Frame, Gpu, Target } from "vgpu";
import type { HeroModule, HeroPalette } from "@/components/hero/gpu-stage";
import compositeShader from "./composite.wgsl";
import { layoutFor, type Layout } from "./layout";
import { createLight } from "./light";
import { run } from "./run";

/** Soft saturation of the light; a single fresh trail sits near half of its colour. */
const GAIN = 0.7;

type Palette = Pick<HeroPalette, "teal" | "orange">;
const look = (p: Palette) => ({ teal: [...p.teal, 1], orange: [...p.orange, 1], gain: GAIN, opacity: 1 });

export function createExposure(gpu: Gpu, layout: Layout, palette: Palette, size: readonly [number, number]) {
  const ex = createLight(gpu, layout, size);
  const develop = effect(gpu, compositeShader, { set: { light: ex.light, look: look(palette) } });
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
      f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => p.draw(develop));
    },
    setPalette: (p: Palette) => develop.set({ look: look(p) }),
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
