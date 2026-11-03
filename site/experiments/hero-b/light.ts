// The long exposure itself, shared by both variants. A feedback target holds
// the light: each frame it is multiplied by exp(-dt/tau) (fade.wgsl, blend
// only) and the light laid down since the last frame is added (deposit.wgsl,
// additive). Because every deposit carries its own age, the whole exposure at
// any moment can also be rebuilt in one draw: that is the first frame, the
// state after a resize, and what the poster bakes.
import { draw, effect, geometry, target } from "vgpu";
import type { Frame, Gpu } from "vgpu";
import depositShader from "./deposit.wgsl";
import fadeShader from "./fade.wgsl";
import type { Layout } from "./layout";
import { createTraffic, Deposits, emit, exposure, POSTER_T, STRIDE, TAU } from "./schedule";

/** Instances for a full rebuild of the exposure (about 16k on the wide stage). */
const CAPACITY = 32768;

/** Pinned locations: the Next.js .wgsl loader minifies, so input names can't be matched. */
const INSTANCE = {
  ends: { format: "float32x4", location: 0 },
  path: { format: "float32x4", location: 1 },
  look: { format: "float32x4", location: 2 },
} as const;

export function createLight(gpu: Gpu, layout: Layout, size: readonly [number, number], opts: { through?: boolean } = {}) {
  const light = target(gpu, { size: [size[0], size[1]], format: "rgba16float", label: "hero-b.light" });
  // One instance per deposit, as a vertex stream (works on adapters without vertex-stage storage).
  const segs = geometry(gpu, {
    vertexCount: 6,
    buffers: [{ stepMode: "instance", data: new Float32Array(CAPACITY * STRIDE), attributes: INSTANCE }],
  });
  const view = (w: number) => ({ size: [layout.width, layout.height], px: w / layout.width, tau: TAU, halo: layout.halo, halo_gain: 0.035 });
  const deposit = draw(gpu, { shader: depositShader, geometry: segs, blend: "additive", set: { view: view(size[0]) } });
  const fade = effect(gpu, fadeShader, { blend: { color: { src: "zero", dst: "src-alpha" } }, set: { fade: { keep: 1 } } });
  const traffic = createTraffic(layout, 7, opts);
  const batch = new Deposits(CAPACITY);
  let t = POSTER_T;
  let rebuild = true;

  /** Advance time by `dt` and encode the light pass: a full rebuild, or fade and add what moved. */
  function encode(f: Frame, dt: number) {
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
  }

  return {
    light,
    layout,
    traffic,
    get time() {
      return t;
    },
    get count() {
      return batch.count;
    },
    compile: () => Promise.all([deposit.compile(light), fade.compile(light)]),
    encode,
    resize(w: number, h: number) {
      light.resize([w, h]);
      deposit.set({ view: view(w) });
      rebuild = true;
    },
    dispose() {
      deposit.dispose();
      segs.destroy();
      fade.dispose();
    },
  };
}
