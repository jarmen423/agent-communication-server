// Concept C's GPU module (loaded by GpuStage after first paint, WebGPU only).
// The tape starts frozen at the poster moment, holds while the canvas fades in
// over the poster, then the paper "motor" spins up. Labels stay in the SVG
// overlay; this module only slides their strips along with the tape.
import { frame, frameLoop, init, surface } from "vgpu";
import type { Frame, FrameLoopHandle } from "vgpu";
import type { HeroModule } from "@/components/hero/gpu-stage";
import marksShader from "./marks.wgsl";
import paperShader from "./paper.wgsl";
import { T0, TALL_TAPE, WIDE_TAPE, buildMarks, packMarks, tapeLabels, type Layout, type TapeLabel } from "./schedule";
import { createTape } from "./tape";

const TONE: Record<TapeLabel["tone"], string> = { muted: "var(--lane-muted)", orange: "var(--lane-orange)", ink: "var(--lane-ink)" };

/** Slides the overlay's label strips with the tape; rebuilds their text once per tick. */
function labelRider(root: Element | null, L: Layout) {
  const ticks = root?.querySelector<SVGGElement>('[data-hc-ride="ticks"]') ?? null;
  const events = root?.querySelector<SVGGElement>('[data-hc-ride="events"]') ?? null;
  let base = T0;
  const fill = (g: SVGGElement | null, labels: TapeLabel[]) => {
    if (!g) return;
    Array.from(g.children).forEach((el, i) => {
      const l = labels[i];
      if (!l) return void (el.textContent = "");
      el.setAttribute("x", l.x.toFixed(2));
      el.setAttribute("y", l.y.toFixed(2));
      el.setAttribute("fill", TONE[l.tone]);
      el.setAttribute("text-anchor", l.anchor);
      if (el.textContent !== l.text) el.textContent = l.text;
    });
  };
  const rebase = (at: number) => {
    base = at;
    const next = tapeLabels(at, L);
    fill(ticks, next.ticks);
    fill(events, next.events);
  };
  const move = (now: number) => {
    if (now - base >= L.labelEvery) rebase(now);
    const tx = `translate(${(-(now - base) * L.pps).toFixed(2)} 0)`;
    ticks?.setAttribute("transform", tx);
    events?.setAttribute("transform", tx);
  };
  return { move, reset: () => (rebase(T0), move(T0)) };
}

const smoothstep = (a: number, b: number, x: number) => {
  const t = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return t * t * (3 - 2 * t);
};

export const startHero: HeroModule["startHero"] = async (canvas, { scene, palette }) => {
  const L = scene.compact ? TALL_TAPE : WIDE_TAPE;
  const gpu = await init();
  try {
    const s = surface(gpu, canvas, { dpr: [1, 2], clearColor: [0, 0, 0, 0] });
    const tape = createTape(gpu, { paper: paperShader, marks: marksShader }, L, palette);
    await tape.compile(s);

    const root = canvas.closest("[data-hc-stage]");
    const labels = labelRider(root, L);
    let now = T0;
    let run = 0;
    let last = 0;
    const paint = (f: Frame) => {
      const n = packMarks(buildMarks(now, L), tape.items);
      f.pass(s, (pass) => tape.draw(pass, now, s.size[0] / L.width, n));
    };
    const tick = (f: Frame) => {
      const t = performance.now();
      const dt = last ? Math.min(0.1, (t - last) / 1000) : 0;
      last = t;
      run += dt;
      // Hold the poster moment while the canvas fades in (600 ms), then ease the paper up to speed.
      now += dt * smoothstep(0.7, 2.0, run);
      paint(f);
      labels.move(now);
    };

    frame(gpu, paint); // the first frame is submitted before we resolve

    let loop: FrameLoopHandle | null = null;
    let offscreen = false;
    let userPaused = false;
    const sync = () => {
      const stop = offscreen || userPaused;
      if (stop && loop) {
        loop.stop();
        loop = null;
      } else if (!stop && !loop) {
        last = 0;
        loop = frameLoop(gpu, tick);
      }
    };
    const onPause = (e: Event) => {
      userPaused = Boolean((e as CustomEvent<boolean>).detail);
      sync();
    };
    root?.addEventListener("hc-pause", onPause);
    userPaused = root?.getAttribute("data-paused") === "true";
    sync();

    return {
      setPaused(p) {
        offscreen = p;
        sync();
      },
      setPalette(p) {
        tape.setPalette(p);
        if (!loop) frame(gpu, paint);
      },
      dispose() {
        loop?.stop();
        loop = null;
        root?.removeEventListener("hc-pause", onPause);
        labels.reset();
        tape.dispose();
        gpu.dispose();
      },
    };
  } catch (err) {
    gpu.dispose();
    throw err;
  }
};
