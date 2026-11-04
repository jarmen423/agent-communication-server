// Light per character cell for the ASCII web, on the CPU. web-cells.wgsl is
// the same maths on the GPU; the server poster uses this one, so the still
// and the first live frame agree. Also the field's text and the look
// constants both renderers share.
import { createWeb, WEB, WEB_POSTER_T, type WebFrame, type WebLayout } from "./web-model";

/**
 * Field alpha `base` (a quiet texture), how much a live line lifts its
 * characters, pulse gain and how fast a lit cell takes on its colour.
 */
export const WEB_LOOK = { base: 0.06, liftAlpha: 0.24, lift: 0.75, ripple: 0.32, click: 1.4, gain: 1.4, litScale: 1.1, hue: 4, ink: 0.9, ahead: 12 };
/** Around the copy the field yields: lines, pulses and rings fall to `floor` (the quiet texture stays). */
export const YIELD = { floor: 0.15, radius: 40 };

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
/** Spaces under a pulse step up this ramp, so a signal never breaks between words. */
export const WEB_RAMP = { chars: [".", ":", "-", "="], at: [0.12, 0.3, 0.5, 0.72] };

/** Proximity width of a line: about one character row. */
export const lineSigma = (l: WebLayout) => (l.height / l.grid[1]) * 0.4;

/**
 * A line's light is negligible beyond 4 sigma (e^-8), so cells outside its
 * inflated bounding box skip it: the same early-out runs in web-cells.wgsl,
 * which keeps the poster and the live frame exact mirrors.
 */
export function lineBox(pts: Float32Array, l: WebLayout): [number, number, number, number] {
  const reach = lineSigma(l) * 4;
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (let j = 0; j < pts.length; j += 2) {
    x0 = Math.min(x0, pts[j]);
    y0 = Math.min(y0, pts[j + 1]);
    x1 = Math.max(x1, pts[j]);
    y1 = Math.max(y1, pts[j + 1]);
  }
  return [x0 - reach, y0 - reach, x1 + reach, y1 + reach];
}

/** [sent, received, neutral, yield] at every cell centre (row-major). */
export function cellLight(l: WebLayout, f: WebFrame): Float32Array {
  const [cols, rows] = l.grid;
  const cw = l.width / cols;
  const ch = l.height / rows;
  const sig2 = 2 * lineSigma(l) ** 2;
  const out = new Float32Array(cols * rows * 4);
  const boxes = f.lines.map((line) => lineBox(line.pts, l));
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const ux = (c + 0.5) * cw;
      const uy = (r + 0.5) * ch;
      let sent = 0, recv = 0, lift = 0;
      for (let li = 0; li < f.lines.length; li++) {
        const line = f.lines[li];
        const box = boxes[li];
        if (ux < box[0] || uy < box[1] || ux > box[2] || uy > box[3]) continue;
        const p = line.pts;
        let best = Infinity, bestS = 0, run = 0;
        for (let j = 0; j + 3 < p.length; j += 2) {
          const ax = p[j], ay = p[j + 1], bx = p[j + 2] - ax, by = p[j + 3] - ay;
          const len2 = bx * bx + by * by || 1e-6;
          const t = Math.min(1, Math.max(0, ((ux - ax) * bx + (uy - ay) * by) / len2));
          const dx = ux - ax - bx * t, dy = uy - ay - by * t;
          const d2 = dx * dx + dy * dy;
          if (d2 < best) {
            best = d2;
            bestS = run + t * Math.sqrt(len2);
          }
          run += Math.sqrt(len2);
        }
        const prox = Math.exp(-best / sig2);
        lift += line.alive * prox;
        for (const [s, amp, dir, tail] of line.pulses) {
          const ds = (bestS - s) * dir; // > 0: not reached yet; < 0: already passed
          const shape = ds > 0 ? Math.exp(-(ds * ds) / (2 * WEB_LOOK.ahead ** 2)) : Math.exp(ds / tail);
          if (amp > 0) sent += amp * prox * shape;
          else recv += -amp * prox * shape;
        }
      }
      const d = Math.hypot(ux - l.hub.cx, uy - l.hub.cy);
      let ripple = 0;
      for (const [radius, amp, width] of f.ripples) ripple += amp * Math.exp(-(((d - radius) / width) ** 2));
      let click = 0;
      for (const [cx, cy, radius, amp] of f.clicks) {
        const x = (Math.hypot(ux - cx, uy - cy) - radius) / (26 + radius * 0.05);
        click += amp * Math.exp(-x * x);
      }
      const y = yieldAt(ux, uy, f.mask);
      const k = YIELD.floor + (1 - YIELD.floor) * y;
      const i = (r * cols + c) * 4;
      out[i] = sent * k;
      out[i + 1] = recv * k;
      out[i + 2] = (lift * WEB_LOOK.lift + ripple * WEB_LOOK.ripple + click * WEB_LOOK.click) * k;
      out[i + 3] = y;
    }
  }
  return out;
}

/** A cell's look from its light: alpha, colour weights and whether a space steps up the ramp. */
export function cellLook(sent: number, recv: number, neutral: number) {
  const es = 1 - Math.exp(-WEB_LOOK.gain * sent);
  const er = 1 - Math.exp(-WEB_LOOK.gain * recv);
  const a = 1 - (1 - es) * (1 - er);
  const n = 1 - Math.exp(-neutral); // soft: lines converging on the hub never stack into loud text
  const quiet = WEB_LOOK.base + WEB_LOOK.liftAlpha * n;
  return { a, share: er / Math.max(es + er, 1e-5), n, alpha: quiet + (1 - quiet) * Math.min(1, a * WEB_LOOK.litScale), hue: Math.min(1, a * WEB_LOOK.hue) };
}

export function rampAt(ch: string, a: number) {
  if (ch !== " ") return ch;
  let g = " ";
  WEB_RAMP.at.forEach((t, i) => {
    if (a >= t) g = WEB_RAMP.chars[i];
  });
  return g;
}

const PROMPTS = ["Fix the failing router test", "Add a changelog entry", "Review the presence handler", "Write tests for the merge gate", "Update the docs index", "Profile hub-server startup", "Bump async-nats"];
const RESULTS = ["1 file changed, tests pass", "2 files changed", "entry added", "no issues found", "PR ready for review"];
const STEPS = ["reading files", "cargo test", "editing", "npm run lint", "writing summary"];

/**
 * The web's own traffic as log records, in the reply contract's shapes: the task
 * DM (subject → inbox), status `working`, event `started`, progress events and
 * one result per task, plus a presence beat.
 */
function webRecords(): string[] {
  const web = createWeb(WEB.M);
  web.advance(WEB_POSTER_T + 60);
  const out: string[] = [];
  web.lines().forEach((x, i) => {
    const id = x.task.slice(5);
    let tick = 0;
    for (const p of x.pulses) {
      if (p.kind === "task") out.push(`hub.pub.${x.from}.${x.task} -> channel.inbox.${x.to} {"prompt":"${PROMPTS[i % PROMPTS.length]}","task_channel":"${x.task}"}`);
      else if (p.kind === "result") out.push(`channel.${x.task} {"status":"done","task_id":"${id}","result":"${RESULTS[i % RESULTS.length]}","error":null}  hub.presence {"identity":"${x.to}"}`);
      else if (tick++ === 0) out.push(`channel.${x.task} {"status":"working","task_id":"${id}"}`);
      else if (tick === 2) out.push(`channel.${x.task} {"event_type":"started","data":{}}`);
      else out.push(`hub.pub.${x.to}.${x.task} {"event_type":"progress","data":{"step":"${STEPS[(i + tick) % STEPS.length]}"}}`);
    }
  });
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
