// Concept B GPU layer. A feedback target holds the exposure: each frame it is
// multiplied by exp(-dt/tau) (fade.wgsl, blend only) and the light laid down
// since the last frame is added (deposit.wgsl, additive). composite.wgsl
// develops it into brand colours over the lane band. Because every deposit
// carries its own age, the whole exposure at any moment can also be rebuilt in
// one draw: that is the first frame, and the same call bakes the poster.
import { clock, draw, effect, frame, frameLoop, geometry, init, surface, target } from "vgpu";
import type { Frame, Gpu, Target } from "vgpu";
import type { HeroModule, HeroPalette } from "@/components/hero/gpu-stage";
import compositeShader from "./composite.wgsl";
import depositShader from "./deposit.wgsl";
import fadeShader from "./fade.wgsl";
import { layoutFor, type Layout } from "./layout";
import { createTraffic, Deposits, emit, exposure, POSTER_T, STRIDE, TAU } from "./schedule";

/** Instances for a full rebuild of the exposure (about 16k on the wide stage). */
const CAPACITY = 32768;
/** Soft saturation of the light; a single fresh trail sits near half of its colour. */
const GAIN = 0.7;

/** Pinned locations: the Next.js .wgsl loader minifies, so input names can't be matched. */
const INSTANCE = {
  ends: { format: "float32x4", location: 0 },
  path: { format: "float32x4", location: 1 },
  look: { format: "float32x4", location: 2 },
} as const;

type Palette = Pick<HeroPalette, "teal" | "orange">;
const look = (p: Palette) => ({ teal: [...p.teal, 1], orange: [...p.orange, 1], gain: GAIN, opacity: 1 });

export function createExposure(gpu: Gpu, layout: Layout, palette: Palette, size: readonly [number, number]) {
  const light = target(gpu, { size: [size[0], size[1]], format: "rgba16float", label: "hero-b.light" });
  // One instance per deposit, as a vertex stream (works on adapters without vertex-stage storage).
  const segs = geometry(gpu, {
    vertexCount: 6,
    buffers: [{ stepMode: "instance", data: new Float32Array(CAPACITY * STRIDE), attributes: INSTANCE }],
  });
  const view = (w: number) => ({ size: [layout.width, layout.height], px: w / layout.width, tau: TAU, halo: layout.halo, halo_gain: 0.035 });
  const deposit = draw(gpu, { shader: depositShader, geometry: segs, blend: "additive", set: { view: view(size[0]) } });
  const fade = effect(gpu, fadeShader, { blend: { color: { src: "zero", dst: "src-alpha" } }, set: { fade: { keep: 1 } } });
  const develop = effect(gpu, compositeShader, { set: { light, look: look(palette) } });
  const traffic = createTraffic(layout);
  const batch = new Deposits(CAPACITY);
  let t = POSTER_T;
  let rebuild = true;

  /** Encode one frame: rebuild the exposure (first frame, after a resize) or fade and add what moved. */
  function encode(f: Frame, out: Target, dt: number) {
    const t0 = t;
    t += dt;
    if (rebuild) exposure(traffic, layout, t, batch);
    else {
      batch.reset();
      for (const m of traffic.live(t)) emit(m, t0, t, batch, layout);
    }
    if (batch.count) segs.write(batch.data.subarray(0, batch.count * STRIDE));
    fade.set({ fade: { keep: Math.exp(-dt / TAU) } });
    const fresh = rebuild;
    rebuild = false;
    f.pass({ target: light, clear: fresh ? [0, 0, 0, 0] : false }, (p) => {
      if (!fresh) p.draw(fade);
      if (batch.count) p.draw(deposit, { instances: batch.count });
    });
    f.pass({ target: out, clear: [0, 0, 0, 0] }, (p) => p.draw(develop));
  }

  return {
    layout,
    traffic,
    get time() {
      return t;
    },
    get count() {
      return batch.count;
    },
    compile: (out: Target) => Promise.all([deposit.compile(light), fade.compile(light), develop.compile(out)]),
    encode,
    resize(w: number, h: number) {
      light.resize([w, h]);
      deposit.set({ view: view(w) });
      rebuild = true;
    },
    setPalette: (p: Palette) => develop.set({ look: look(p) }),
    dispose() {
      deposit.dispose();
      segs.destroy();
      fade.dispose();
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
    const errors: unknown[] = [];
    const unsubscribe = gpu.onError((e) => errors.push(e));
    frame(gpu, (f) => ex.encode(f, out, 0));
    await gpu.settled();
    unsubscribe();
    if (errors.length) throw errors[0];

    // Two reasons to hold still: GpuStage's (off-screen, hidden tab) and the visitor's (the pause button).
    const held = { stage: false, user: false };
    const time = clock(gpu);
    let loop: { stop(): void } | null = null;
    const sync = () => {
      const run = !held.stage && !held.user;
      if (run && !loop) {
        let first = true;
        loop = frameLoop(gpu, (f) => {
          // A resumed loop's first delta spans the pause; light should not jump.
          const dt = first ? 1 / 60 : Math.min(time.deltaTime, 0.1);
          first = false;
          ex.encode(f, out, dt);
        }, { fps: 60 });
      } else if (!run && loop) {
        loop.stop();
        loop = null;
      }
    };
    const onUser = (e: Event) => {
      held.user = Boolean((e as CustomEvent<boolean>).detail);
      sync();
    };
    canvas.addEventListener("hero:pause", onUser);
    sync();
    return {
      setPaused(p) {
        held.stage = p;
        sync();
      },
      setPalette: (p) => ex.setPalette(p),
      dispose() {
        canvas.removeEventListener("hero:pause", onUser);
        loop?.stop();
        ex.dispose();
        gpu.dispose();
      },
    };
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
