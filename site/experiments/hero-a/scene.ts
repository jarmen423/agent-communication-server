// Concept A, signal box: the browser half. Loaded by <GpuStage> after first
// paint, only where WebGPU exists. Starts at the poster's moment, so the fade
// from poster to live layer shows no jump.
import { effect, frame, frameLoop, init, surface, type FrameLoopHandle } from "vgpu";
import type { HeroModule } from "@/components/hero/gpu-stage";
import { boardValues, liveValues, paint } from "./board";
import { motion } from "./control";
import { buildModel, frameAt, POSTER_T, type FrameState } from "./script";
import shader from "./signal.wgsl";

/** Keeps the poster's live text (node states, route labels) in step with the loop. */
function labels(canvas: HTMLCanvasElement) {
  const root = canvas.closest(".gpu-stage");
  const states = [...(root?.querySelectorAll<SVGTextElement>("[data-ha-state]") ?? [])];
  const routes = [...(root?.querySelectorAll<SVGTextElement>("[data-ha-route]") ?? [])];
  const saved = states.map((el) => el.textContent);
  const savedOn = routes.map((el) => el.hasAttribute("data-on"));
  return {
    sync(f: FrameState) {
      for (const el of states) {
        const text = f.states[el.dataset.haState!];
        if (text && el.textContent !== text) el.textContent = text;
      }
      for (const el of routes) el.toggleAttribute("data-on", !!f.routes[el.dataset.haRoute!]);
    },
    restore() {
      states.forEach((el, i) => (el.textContent = saved[i]));
      routes.forEach((el, i) => el.toggleAttribute("data-on", savedOn[i]));
    },
  };
}

export const startHero: HeroModule["startHero"] = async (canvas, { scene, palette }) => {
  const model = buildModel(scene);
  const gpu = await init();
  let failed: unknown = null;
  const offError = gpu.onError((e) => (failed ??= e));
  try {
    const s = surface(gpu, canvas, { dpr: [1, 2] });
    let t = POSTER_T;
    const fx = effect(gpu, shader, { set: { board: boardValues(model, palette, 1), live: liveValues(frameAt(model, t)) } });
    const offResize = s.onResize(({ width }) => {
      const px = width / scene.width;
      fx.set({ board: { view: [scene.width, scene.height, px, 1 / px] } });
    });
    await fx.compile(s);

    const text = labels(canvas);
    const draw = () => {
      const f = frameAt(model, t);
      fx.set({ live: liveValues(f) });
      frame(gpu, (fr) => fr.pass(s, fx));
      text.sync(f);
    };
    draw();
    await gpu.settled();
    if (failed) throw failed;

    let loop: FrameLoopHandle | null = null;
    let hidden = false;
    let last = 0;
    const run = () => {
      const want = !hidden && !motion.paused;
      if (want && !loop) {
        last = performance.now();
        loop = frameLoop(gpu, (fr) => {
          const now = performance.now();
          t += Math.min(0.25, (now - last) / 1000); // capped below the shortest leg, so a stall never skips an envelope
          last = now;
          const f = frameAt(model, t);
          fx.set({ live: liveValues(f) });
          fr.pass(s, fx);
          text.sync(f);
        });
      } else if (!want && loop) {
        loop.stop();
        loop = null;
      }
    };
    const offMotion = motion.subscribe(run);
    run();

    return {
      setPaused(p) {
        hidden = p;
        run();
      },
      setPalette(pal) {
        fx.set({ board: paint(pal) });
        if (!loop) draw(); // a paused board still follows the theme
      },
      dispose() {
        loop?.stop();
        loop = null;
        offMotion();
        offResize();
        offError();
        text.restore();
        gpu.dispose();
      },
    };
  } catch (err) {
    offError();
    gpu.dispose();
    throw err;
  }
};
