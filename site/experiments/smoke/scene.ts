import { clock, effect, frameLoop, init, surface } from "vgpu";
import type { HeroModule } from "@/components/hero/gpu-stage";
import shader from "./smoke.wgsl";

export const startHero: HeroModule["startHero"] = async (canvas, { palette }) => {
  const gpu = await init();
  const s = surface(gpu, canvas, { dpr: [1, 2] });
  const fx = effect(gpu, shader, { set: { params: { time: 0, teal: palette.teal } } });
  const t = clock(gpu);
  let loop = frameLoop(gpu, (frame) => {
    fx.set({ params: { time: t.time } });
    frame.pass(s, fx);
  });
  return {
    setPaused(p) {
      if (p) loop.stop();
      else loop = frameLoop(gpu, (frame) => { fx.set({ params: { time: t.time } }); frame.pass(s, fx); });
    },
    setPalette(pal) { fx.set({ params: { teal: pal.teal } }); },
    dispose() { loop.stop(); gpu.dispose(); },
  };
};
