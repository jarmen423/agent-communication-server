// The live lifecycle both variants share: render the first frame and make sure
// it compiled cleanly (else throw, so GpuStage keeps the poster), then run the
// frame loop with two reasons to hold still: GpuStage's (off-screen, hidden
// tab) and the visitor's (the pause button, as a `hero:pause` event).
import { clock, frame, frameLoop } from "vgpu";
import type { Frame, Gpu } from "vgpu";
import type { HeroPalette, HeroRenderer } from "@/components/hero/gpu-stage";

export type Layer = {
  encode(f: Frame, dt: number): void;
  setPalette(p: HeroPalette): void;
  dispose(): void;
};

export async function run(gpu: Gpu, canvas: HTMLCanvasElement, layer: Layer): Promise<HeroRenderer> {
  const errors: unknown[] = [];
  const unsubscribe = gpu.onError((e) => errors.push(e));
  frame(gpu, (f) => layer.encode(f, 0));
  await gpu.settled();
  unsubscribe();
  if (errors.length) throw errors[0];

  const held = { stage: false, user: false };
  const time = clock(gpu);
  let loop: { stop(): void } | null = null;
  const sync = () => {
    const go = !held.stage && !held.user;
    if (go && !loop) {
      let first = true;
      loop = frameLoop(gpu, (f) => {
        // A resumed loop's first delta spans the pause; light should not jump.
        const dt = first ? 1 / 60 : Math.min(time.deltaTime, 0.1);
        first = false;
        layer.encode(f, dt);
      }, { fps: 60 });
    } else if (!go && loop) {
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
    setPalette: (p) => layer.setPalette(p),
    dispose() {
      canvas.removeEventListener("hero:pause", onUser);
      loop?.stop();
      layer.dispose();
      gpu.dispose();
    },
  };
}
