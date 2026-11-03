// Light per character cell for the ASCII web, on the CPU. web-cells.wgsl is
// the same maths on the GPU; the server poster uses this one, so the still
// and the first live frame agree. Also the field's text and the look
// constants both renderers share.
import { record } from "./field";
import { wideLayout } from "./layout";
import { createTraffic, POSTER_T } from "./schedule";
import type { WebFrame, WebLayout } from "./web-model";

/**
 * Field alpha `base` (a quiet texture), how much a live line lifts its
 * characters, pulse gain and how fast a lit cell takes on its colour.
 */
export const WEB_LOOK = { base: 0.06, liftAlpha: 0.24, lift: 0.75, ripple: 0.32, gain: 1.4, litScale: 1.1, hue: 4, ink: 0.9, ahead: 12 };
/** Spaces under a pulse step up this ramp, so a signal never breaks between words. */
export const WEB_RAMP = { chars: [".", ":", "-", "="], at: [0.12, 0.3, 0.5, 0.72] };

/** Proximity width of a line: about one character row. */
export const lineSigma = (l: WebLayout) => (l.height / l.grid[1]) * 0.4;

/** [sent, received, neutral] light at every cell centre (row-major). */
export function cellLight(l: WebLayout, f: WebFrame): Float32Array {
  const [cols, rows] = l.grid;
  const cw = l.width / cols;
  const ch = l.height / rows;
  const sig2 = 2 * lineSigma(l) ** 2;
  const out = new Float32Array(cols * rows * 3);
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const ux = (c + 0.5) * cw;
      const uy = (r + 0.5) * ch;
      let sent = 0, recv = 0, lift = 0;
      for (const line of f.lines) {
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
      const i = (r * cols + c) * 3;
      out[i] = sent;
      out[i + 1] = recv;
      out[i + 2] = lift * WEB_LOOK.lift + ripple * WEB_LOOK.ripple;
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

/** The field's text: the hub's own traffic log, wrapped row by row. Deterministic. */
export function webRows(cols: number, rows: number): string[] {
  const traffic = createTraffic(wideLayout(), 7, {});
  traffic.advance(POSTER_T + 30);
  const recs = [...traffic.pending()].sort((a, b) => a.t0 - b.t0).map((m, i) => record(m, i));
  return Array.from({ length: rows }, (_, r) => {
    let line = "";
    for (let k = (r * 11) % recs.length; line.length < cols + 40; k = (k + 1) % recs.length) line += recs[k] + "  ";
    const skip = (r * 17) % 31;
    return line.slice(skip, skip + cols);
  });
}
