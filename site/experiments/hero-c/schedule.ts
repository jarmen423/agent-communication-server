// Concept C, "strip chart": the hub's traffic as a looping script, the layout of
// the tape, and one function that turns a moment into marks. The poster (SVG)
// and the GPU layer both draw from `buildMarks`, so the live layer's first frame
// is the poster. Pure data: no DOM, no vgpu (the Node test imports it directly).

export type Row = { id: string; machine: string; y: number; hub?: boolean };

export type Layout = {
  width: number;
  height: number;
  /** Tape left/right edge and the length of the fade at each end (viewBox units). */
  x0: number;
  x1: number;
  fade: number;
  /** The "now" playhead, where the pens write. */
  play: number;
  /** Tape speed, viewBox units per second. */
  pps: number;
  axisY: number;
  bottom: number;
  /** Seconds between labelled time ticks. */
  labelEvery: number;
  rows: Row[];
  compact: boolean;
  /** Mark heights above a row's baseline. */
  h: { sent: number; result: number; event: number; status: number };
};

/** One cycle of the script, in seconds after 12:04:00. The tape loops every PERIOD. */
export const PERIOD = 30;
/** The poster moment, and where the live layer starts. */
export const T0 = 21.6;
/** 12:04:00 as seconds since midnight, for the time ticks. */
export const CLOCK = 12 * 3600 + 4 * 60;

type Leg = { worker: string; status: number[]; events: number[]; result: number; to: "sender" | "hub" };
type Task = { channel: string; from: string; sent: number; legs: Leg[]; labelOn: "sender" | "hub" };

/**
 * The script. Channel names and event kinds are the real ones: a task DM'd from
 * an orchestrator, then status `working`, events `started`/`progress` on the
 * task channel, then exactly one terminal result. The wave is spawned through
 * the hub (`wave.spawn`); hub-server DMs `session_start` to each worker on
 * `wave.<id>.task.<task>` and its orchestrator collects the results.
 */
export const TASKS: Task[] = [
  {
    channel: "#task.2b81d04a",
    from: "claude-web-1",
    sent: 2.2,
    labelOn: "sender",
    legs: [{ worker: "hermes-1", status: [2.27], events: [2.36, 3.9, 5.2], result: 6.4, to: "sender" }],
  },
  {
    channel: "#task.7f3a2c1e",
    from: "me",
    sent: 7.112,
    labelOn: "sender",
    legs: [{ worker: "claude-1", status: [7.18], events: [7.204, 11.0, 14.6, 17.3, 19.902], result: 20.45, to: "sender" }],
  },
  {
    channel: "#wave.docs-pass",
    from: "me",
    sent: 9.004,
    labelOn: "hub",
    legs: [
      { worker: "codex-1", status: [9.06], events: [9.1, 12.8, 16.1, 18.9, 22.6, 25.4], result: 27.2, to: "hub" },
      { worker: "hermes-1", status: [9.09], events: [9.14, 13.6, 17.4, 20.7, 24.1], result: 25.9, to: "hub" },
    ],
  },
];

const AGENTS = {
  me: "laptop",
  "claude-web-1": "claude.ai",
  "claude-1": "workstation",
  "codex-1": "build-box",
  "hermes-1": "home-server",
} as const;
const row = (id: keyof typeof AGENTS, y: number): Row => ({ id, machine: AGENTS[id], y });
const hub = (y: number): Row => ({ id: "hub-server", machine: "", y, hub: true });

/** Desktop, viewBox 1200 × 440: orchestrators above the hub rail, workers below. */
export const WIDE_TAPE: Layout = {
  width: 1200,
  height: 440,
  x0: 226,
  x1: 1188,
  fade: 64,
  play: 1004,
  pps: 36,
  axisY: 40,
  bottom: 398,
  labelEvery: 5,
  compact: false,
  rows: [row("me", 92), row("claude-web-1", 146), hub(204), row("claude-1", 266), row("codex-1", 322), row("hermes-1", 378)],
  h: { sent: 26, result: 36, event: 10, status: 6 },
};

/** Phones, viewBox 360 × 440: one fewer orchestrator, labels under each trace. */
export const TALL_TAPE: Layout = {
  width: 360,
  height: 440,
  x0: 6,
  x1: 356,
  fade: 40,
  play: 296,
  pps: 12,
  axisY: 40,
  bottom: 384,
  labelEvery: 10,
  compact: true,
  rows: [row("me", 104), hub(166), row("claude-1", 236), row("codex-1", 298), row("hermes-1", 360)],
  h: { sent: 24, result: 32, event: 9, status: 6 },
};

// ---- marks ----

export const KIND = { bar: 0, dot: 1, sentTile: 2, recvTile: 3, glow: 4 } as const;
export const COLOR = { teal: 0, orange: 1, ink: 2, muted: 3 } as const;
/** Floats per mark: rect (cx, cy, hw, hh) then style (kind, colour, alpha, unused). */
export const STRIDE = 8;
export const MAX_MARKS = 192;

export type Mark = { kind: number; color: number; alpha: number; cx: number; cy: number; hw: number; hh: number };

const easeOut = (x: number) => 1 - Math.pow(1 - Math.min(1, Math.max(0, x)), 3);

/**
 * Every mark visible at tape time `now`. Marks written in the last few hundred
 * milliseconds grow out of the pen (`grow` = false freezes them, for the poster).
 */
export function buildMarks(now: number, L: Layout, grow = true): Mark[] {
  const out: Mark[] = [];
  const y = (id: string) => L.rows.find((r) => r.id === id)?.y;
  const hubY = L.rows.find((r) => r.hub)!.y;
  const x = (t: number) => L.play - (now - t) * L.pps;
  const oldest = now - (L.play - L.x0 + 40) / L.pps;
  const g = (t: number, d: number) => (grow ? easeOut((now - t) / d) : 1);
  const bar = (c: number, a: number, x0: number, y0: number, x1: number, y1: number) =>
    out.push({ kind: KIND.bar, color: c, alpha: a, cx: (x0 + x1) / 2, cy: (y0 + y1) / 2, hw: Math.abs(x1 - x0) / 2, hh: Math.abs(y1 - y0) / 2 });
  const dot = (c: number, a: number, cx: number, cy: number, r: number) => out.push({ kind: KIND.dot, color: c, alpha: a, cx, cy, hw: r, hh: r });
  const flash = (c: number, t: number, cy: number) => {
    const age = now - t;
    if (grow && age < 0.6) out.push({ kind: KIND.glow, color: c, alpha: 0.4 * (1 - age / 0.6), cx: L.play, cy, hw: 14, hh: 14 });
  };
  // A connector runs from one row to another; it is drawn growing in the travel direction.
  const connector = (c: number, t: number, from: number, to: number) => {
    const k = g(t, 0.32);
    bar(c, 0.5, x(t) - 0.5, from, x(t) + 0.5, from + (to - from) * k);
    return k;
  };
  const flag = (c: number, t: number, base: number, h: number, dir: "sent" | "received") => {
    const k = g(t, 0.22);
    const top = base - h * k;
    bar(c, 1, x(t) - 1, top, x(t) + 1, base);
    const w = L.compact ? 9 : 11;
    const th = L.compact ? 7 : 8;
    const cx = dir === "sent" ? x(t) - 1 + w / 2 : x(t) + 1 - w / 2;
    if (k > 0.5) out.push({ kind: dir === "sent" ? KIND.sentTile : KIND.recvTile, color: c, alpha: 1, cx, cy: top + th / 2, hw: w / 2, hh: th / 2 });
  };

  const cycle = Math.floor(now / PERIOD) * PERIOD;
  for (const base of [cycle - PERIOD, cycle]) {
    for (const task of TASKS) {
      const ys = y(task.from);
      const legs = task.legs.filter((l) => y(l.worker) !== undefined);
      if (ys === undefined || !legs.length) continue;
      const ts = base + task.sent;
      const lastEnd = Math.max(...legs.map((l) => base + l.result));
      if (ts > now || lastEnd < oldest) continue;

      // Busy spans first, under everything: a worker's trace thickens while it works.
      for (const l of legs) {
        const yw = y(l.worker)!;
        const end = Math.min(now, base + l.result);
        bar(COLOR.ink, 0.34, Math.max(x(ts), L.x0 - 20), yw - 1, x(end), yw + 1);
      }

      // Sent: a teal flag on the sender's trace, a connector through the hub to every receiver.
      const yMax = Math.max(...legs.map((l) => y(l.worker)!));
      const k = connector(COLOR.teal, ts, ys, yMax);
      dot(COLOR.teal, 0.95, x(ts), hubY - 3, 2.2);
      for (const l of legs) if (ys + (yMax - ys) * k >= y(l.worker)! - 0.5) dot(COLOR.teal, 1, x(ts), y(l.worker)!, 2.4);
      flag(COLOR.teal, ts, ys, L.h.sent, "sent");
      flash(COLOR.teal, ts, ys);

      for (const l of legs) {
        const yw = y(l.worker)!;
        // Progress: small orange ticks on the worker's trace, a fainter dot on the hub rail.
        for (const [list, h] of [[l.status, L.h.status], [l.events, L.h.event]] as const) {
          for (const p of list) {
            const tp = base + p;
            if (tp > now) continue;
            bar(COLOR.orange, 0.9, x(tp) - 0.75, yw - h * g(tp, 0.16), x(tp) + 0.75, yw);
            dot(COLOR.orange, 0.55, x(tp), hubY + 3, 1.5);
            flash(COLOR.orange, tp, yw);
          }
        }
        // The one result: a taller orange flag and a connector back to whoever collects it.
        const tr = base + l.result;
        if (tr > now) continue;
        const dest = l.to === "sender" ? ys : hubY;
        const kr = connector(COLOR.orange, tr, yw, dest);
        dot(COLOR.orange, 1, x(tr), hubY + 3, 2.2);
        if (kr > 0.98) dot(COLOR.orange, 1, x(tr), dest + (l.to === "hub" ? 3 : 0), 2.4);
        flag(COLOR.orange, tr, yw, L.h.result, "received");
        flash(COLOR.orange, tr, yw);
      }
    }
  }
  return out.slice(0, MAX_MARKS);
}

/** Pack marks for the GPU: STRIDE floats each. Returns the instance count. */
export function packMarks(marks: Mark[], out: Float32Array): number {
  const n = Math.min(marks.length, MAX_MARKS, Math.floor(out.length / STRIDE));
  for (let i = 0; i < n; i++) {
    const m = marks[i];
    out.set([m.cx, m.cy, m.hw, m.hh, m.kind, m.color, m.alpha, 0], i * STRIDE);
  }
  return n;
}

// ---- labels (DOM/SVG, never in the shader) ----

export type TapeLabel = { key: string; x: number; y: number; text: string; tone: "muted" | "orange" | "ink"; anchor: "start" | "middle" | "end" };

const two = (n: number) => String(n).padStart(2, "0");
export function clockText(t: number): string {
  const s = Math.floor(CLOCK + t);
  return `${two(Math.floor(s / 3600) % 24)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}`;
}

/**
 * Labels that ride on the tape, laid out for tape time `at` and valid until
 * `at + labelEvery` (the strip moves left by (now - at) * pps between rebuilds).
 */
export function tapeLabels(at: number, L: Layout): { ticks: TapeLabel[]; events: TapeLabel[] } {
  const x = (t: number) => L.play - (at - t) * L.pps;
  const from = at - (L.play - L.x0) / L.pps - L.labelEvery;
  const until = at + (L.x1 - L.play) / L.pps + L.labelEvery * 2;
  const ticks: TapeLabel[] = [];
  for (let t = Math.ceil(from / L.labelEvery) * L.labelEvery; t <= until; t += L.labelEvery) {
    ticks.push({ key: `t${t}`, x: x(t), y: L.axisY - 12, text: clockText(t), tone: "muted", anchor: "middle" });
  }
  const events: TapeLabel[] = [];
  const y = (id: string) => L.rows.find((r) => r.id === id)?.y;
  const hubY = L.rows.find((r) => r.hub)!.y;
  const cycle = Math.floor(at / PERIOD) * PERIOD;
  for (const base of [cycle - PERIOD, cycle, cycle + PERIOD]) {
    for (const task of TASKS) {
      const ys = y(task.from);
      if (ys === undefined || !task.legs.some((l) => y(l.worker) !== undefined)) continue;
      const ts = base + task.sent;
      if (ts >= from && ts <= at + L.labelEvery * 2) {
        const ly = task.labelOn === "hub" ? hubY + 17 : ys + 17;
        events.push({ key: `${base}${task.channel}`, x: x(ts) + 5, y: ly, text: task.channel, tone: "muted", anchor: "start" });
      }
      for (const l of task.legs) {
        const yw = y(l.worker);
        const tr = base + l.result;
        if (yw === undefined || tr < from || tr > at + L.labelEvery * 2) continue;
        // Left of the received tile, so it is printed the moment the result is.
        const tile = L.compact ? 9 : 11;
        events.push({ key: `${base}${task.channel}${l.worker}`, x: x(tr) - tile - 4, y: yw - L.h.result + 7, text: "done", tone: "orange", anchor: "end" });
      }
    }
  }
  return { ticks, events };
}
