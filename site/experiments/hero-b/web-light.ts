// The ASCII ground's light per character cell, on the CPU. web-cells.wgsl is
// the same maths on the GPU (tests compare the two). Also the field's text
// and the look constants both renderers share.
import { WEB, WEB_POSTER_T, type WebFrame, type WebLayout } from "./web-model";
import { createWebTraffic } from "./web-tracks";

/**
 * Field alpha `base` (a quiet texture), how far neutral light (hub ripples,
 * clicks) lifts a character's alpha, the weights of each, and how far lifted
 * characters move from muted toward ink.
 */
export const WEB_LOOK = { base: 0.06, liftAlpha: 0.24, ripple: 0.32, click: 1.4, torch: 0.4, ink: 0.9 };
/** Around the copy the field yields: tracks, rings and the lamp fall to `floor` (the quiet texture stays). */
export const YIELD = { floor: 0.15, radius: 40 };
/**
 * Above the headline's bottom the tracks yield less (to `floor`), so the
 * reaches show behind the large type, whose halos keep it readable; over
 * `ramp` design units below it the yield returns to YIELD.floor, before the
 * paragraph. Tracks only: the ground keeps YIELD everywhere.
 */
export const HEAD = { floor: 0.6, ramp: 32 };
/**
 * A visitor's click rings yield far less: only right behind the copy block
 * (to `floor`, over a short `feather`), so a click anywhere on the field,
 * next to the headline too, rings clearly. Hub ripples keep YIELD.
 */
export const CLICK_YIELD = { floor: 0.35, feather: 40 };

/** 0 inside the copy block, rising (smoothstep) to 1 at `feather` outside it: no edge anywhere. */
export function yieldAt(x: number, y: number, m: readonly number[]) {
  const hx = (m[2] - m[0]) / 2;
  const hy = (m[3] - m[1]) / 2;
  const r = YIELD.radius;
  const qx = Math.abs(x - (m[0] + hx)) - hx + r;
  const qy = Math.abs(y - (m[1] + hy)) - hy + r;
  const sd = Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
  const u = Math.min(1, Math.max(0, sd / m[4]));
  return u * u * (3 - 2 * u);
}

/** [neutral, yield] at every cell centre (row-major). */
export function cellLight(l: WebLayout, f: WebFrame): Float32Array {
  const [cols, rows] = l.grid;
  const cw = l.width / cols;
  const ch = l.height / rows;
  const out = new Float32Array(cols * rows * 2);
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const ux = (c + 0.5) * cw;
      const uy = (r + 0.5) * ch;
      const d = Math.hypot(ux - l.hub.cx, uy - l.hub.cy);
      let ripple = 0;
      for (const [radius, amp, width] of f.ripples) ripple += amp * Math.exp(-(((d - radius) / width) ** 2));
      let click = 0;
      for (const [cx, cy, radius, amp] of f.clicks) {
        const x = (Math.hypot(ux - cx, uy - cy) - radius) / (26 + radius * 0.05);
        click += amp * Math.exp(-x * x);
      }
      const [tx, ty, tr, ta] = f.torch;
      const torch = ta * Math.exp(-((Math.hypot(ux - tx, uy - ty) / tr) ** 2));
      const y = yieldAt(ux, uy, f.mask);
      const k = YIELD.floor + (1 - YIELD.floor) * y;
      const kc = CLICK_YIELD.floor + (1 - CLICK_YIELD.floor) * yieldAt(ux, uy, [...f.mask.slice(0, 4), CLICK_YIELD.feather]);
      const i = (r * cols + c) * 2;
      out[i] = ripple * WEB_LOOK.ripple * k + (click * WEB_LOOK.click + torch * WEB_LOOK.torch) * kc;
      out[i + 1] = y;
    }
  }
  return out;
}

const PROMPTS = ["Fix the failing router test", "Add a changelog entry", "Review the presence handler", "Write tests for the merge gate", "Update the docs index", "Profile hub-server startup", "Bump async-nats"];
const RESULTS = ["1 file changed, tests pass", "2 files changed", "entry added", "no issues found", "PR ready for review"];

/**
 * The web's own traffic as log records, in the reply contract's shapes: the task
 * DM (subject → inbox), status `working`, event `started`, progress events and
 * one result per task, plus a presence beat.
 */
function webRecords(): string[] {
  const traffic = createWebTraffic(WEB.M);
  traffic.advance(WEB_POSTER_T + 60);
  const out: string[] = [];
  let n = 0;
  for (const m of traffic.pending()) {
    const id = m.task.slice(5);
    if (m.kind === "task") out.push(`hub.pub.${m.from}.${m.task} -> channel.inbox.${m.to} {"prompt":"${PROMPTS[n++ % PROMPTS.length]}","task_channel":"${m.task}"}`);
    else if (m.kind === "result") out.push(`channel.${m.task} {"status":"done","task_id":"${id}","result":"${RESULTS[n % RESULTS.length]}","error":null}  hub.presence {"identity":"${m.from}"}`);
    else if (m.kind === "status") out.push(`channel.${m.task} {"status":"working","task_id":"${id}"}`);
    else if (m.detail === "started") out.push(`channel.${m.task} {"event_type":"started","data":{}}`);
    else out.push(`hub.pub.${m.from}.${m.task} {"event_type":"progress","data":{"step":"${m.detail.replace("progress · ", "")}"}}`);
  }
  return out;
}

/** The field's text: the web's own traffic log, wrapped row by row. Deterministic. */
export function webRows(cols: number, rows: number): string[] {
  const recs = webRecords();
  return Array.from({ length: rows }, (_, r) => {
    let line = "";
    for (let k = (r * 11) % recs.length; line.length < cols + 40; k = (k + 1) % recs.length) line += recs[k] + "  ";
    const skip = (r * 17) % 31;
    return line.slice(skip, skip + cols);
  });
}
