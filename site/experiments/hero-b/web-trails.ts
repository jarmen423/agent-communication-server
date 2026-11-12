// Plain B's long exposure, running on the web's tracks: light.ts (feedback +
// deposits) fed by createWebTraffic, developed by web-trails.wgsl. Shared by
// the live layer (cover-mapped, half resolution) and the poster bake (the
// design space itself).
import { effect, sampler } from "vgpu";
import type { Gpu, Target } from "vgpu";
import { createLight } from "./light";
import { FEATHER, WEB_POSTER_T, type WebLayout } from "./web-model";
import { HEAD, YIELD } from "./web-light";
import { createWebTraffic } from "./web-tracks";
import trailsShader from "./web-trails.wgsl";

/** B's soft saturation (0.7); sent gets more, since teal is the darker hue on the dark ground. */
const GAIN = { sent: 1.5, received: 1.0 };
/** Chroma boost on the developed light (1 = the tokens as they are). */
const VIVID = 1.35;

type Rgb = readonly [number, number, number];

export function createTrails(gpu: Gpu, w: WebLayout, lightSize: readonly [number, number], opts: { cover: boolean; palette: { teal: Rgb; orange: Rgb } }) {
  const traffic = createWebTraffic(w);
  const ex = createLight(gpu, traffic.layout, lightSize, { through: true, traffic, cover: opts.cover, t0: WEB_POSTER_T });
  let palette = opts.palette;
  let copy: readonly number[] = w.copy;
  let out: readonly [number, number] = lightSize;
  const uniform = () => {
    const k = opts.cover ? Math.max(out[0] / w.width, out[1] / w.height) : out[0] / w.width;
    return {
      teal: [...palette.teal, 1],
      orange: [...palette.orange, 1],
      size: [out[0], out[1]],
      gain: [GAIN.sent, GAIN.received],
      map: [k, (out[0] - w.width * k) / 2, (out[1] - w.height * k) / 2, 0],
      mask: copy.slice(0, 4),
      mask2: [FEATHER, YIELD.floor, YIELD.radius, VIVID],
      // No headline bottom given: the head zone is empty (the whole block yields fully).
      head: [HEAD.floor, copy[4] ?? copy[1], HEAD.ramp, 0],
    };
  };
  const develop = effect(gpu, trailsShader, {
    blend: "premultiplied",
    set: { light: ex.light, samp: sampler(gpu, { minFilter: "linear", magFilter: "linear" }), trails: uniform() },
  });
  return {
    ex,
    traffic,
    develop,
    compile: (target: Target) => Promise.all([ex.compile(), develop.compile(target)]),
    /** Output size changed (the light target follows at its own scale). */
    resize(output: readonly [number, number], light: readonly [number, number]) {
      out = output;
      ex.resize(light[0], light[1]);
      develop.set({ trails: uniform() });
    },
    setOutput(output: readonly [number, number]) {
      out = output;
      develop.set({ trails: uniform() });
    },
    setCopy(rect: readonly number[]) {
      copy = rect;
      develop.set({ trails: uniform() });
    },
    setPalette(p: { teal: Rgb; orange: Rgb }) {
      palette = p;
      develop.set({ trails: uniform() });
    },
    dispose() {
      ex.dispose();
      develop.dispose();
    },
  };
}
