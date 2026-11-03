// The GPU side of the strip chart: one fullscreen effect for the paper and pens,
// one instanced draw for the ink. Knows nothing about the script; it is handed
// packed marks each frame. Shaders are passed in so the Node test can prepare
// them without a bundler.
import { Uniform, draw, effect } from "vgpu";
import type { FramePass, Gpu, ShaderSource, Target, TargetSignature } from "vgpu";
import type { HeroPalette } from "@/components/hero/gpu-stage";
import type { Layout } from "./schedule";

export type TapeShaders = { paper: ShaderSource; marks: ShaderSource };

// marks.wgsl's uniform, written as raw bytes once per frame: view, tape, four
// colours, then 384 vec4s (two per mark). All vec4f, so no padding to manage.
const HEADER = 24;
const ITEMS = 384 * 4;

const rgba = (rgb: [number, number, number]) => [rgb[0], rgb[1], rgb[2], 1];
const paperColours = (p: HeroPalette) => ({ ink: rgba(p.laneInk), line: rgba(p.laneLine), muted: rgba(p.laneMuted) });

export function createTape(gpu: Gpu, shaders: TapeShaders, L: Layout, palette: HeroPalette) {
  const rows = Array.from({ length: 8 }, (_, i) => (L.rows[i] ? [L.rows[i].y, L.rows[i].hub ? 1 : 0, 0, 0] : [0, 0, 0, 0]));
  const paper = effect(gpu, shaders.paper, {
    set: {
      paper: {
        view: [L.width, L.height, 1, L.play],
        tape: [L.x0, L.x1, L.fade, L.pps],
        axis: [L.axisY, L.bottom, 0, 5],
        info: [L.rows.length, 0, 0, 0],
        rows,
        ...paperColours(palette),
      },
    },
  });
  const ink = draw(gpu, {
    shader: shaders.marks,
    label: "hero-c.marks",
    vertices: 6,
    instances: 0,
    blend: "premultiplied",
  });
  const bytes = new Float32Array(HEADER + ITEMS);
  const uniform = new Uniform(gpu.device, { size: bytes.byteLength, label: "hero-c.marks", bindGroupLayout: ink.layout(0) });
  const colours = (p: HeroPalette) => bytes.set([...rgba(p.teal), ...rgba(p.orange), ...rgba(p.laneInk), ...rgba(p.laneMuted)], 8);
  bytes.set([L.width, L.height, 1, 0, L.x0, L.x1, L.fade, 0]);
  colours(palette);
  uniform.write(bytes);
  ink.set({ marks: uniform });

  return {
    /** Pack marks here (STRIDE floats each) before `draw`. */
    items: bytes.subarray(HEADER),
    /** Warm both pipelines; rejects if either fails to compile (the poster stays). */
    async compile(target: Target | TargetSignature) {
      await Promise.all([paper.compile(target), ink.compile(target)]);
    },
    /** Encode one frame: `pxPerUnit` is device pixels per viewBox unit. */
    draw(pass: FramePass, now: number, pxPerUnit: number, count: number) {
      paper.set({ paper: { view: [L.width, L.height, pxPerUnit, L.play], axis: [L.axisY, L.bottom, now, 5] } });
      pass.draw(paper);
      if (count > 0) {
        bytes[2] = pxPerUnit;
        uniform.write(bytes, 0);
        pass.draw(ink, { instances: count });
      }
    },
    setPalette(p: HeroPalette) {
      paper.set({ paper: paperColours(p) });
      colours(p);
      uniform.write(bytes);
    },
    dispose() {
      paper.dispose();
      ink.dispose();
      uniform.dispose();
    },
  };
}

export type Tape = ReturnType<typeof createTape>;
