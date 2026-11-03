// The traffic: a deterministic stream of delegated tasks, each following the
// reply contract (refocus.md §6). One teal task goes out (orchestrator → hub →
// worker), a run of short orange ticks comes back (status `working`, events
// `started` and `progress`), then exactly one brighter, longer orange result.
// Every streak on screen is one of these messages between two named agents.
import { crossing, type Agent, type Lane, type Layout } from "./layout";

export type Kind = "task" | "status" | "event" | "result";
export type Msg = {
  task: string;
  from: string;
  to: string;
  kind: Kind;
  detail: string;
  /** Teal (sent, 0) or orange (received, 1). */
  channel: 0 | 1;
  t0: number;
  speed: number;
  a: Lane;
  b: Lane;
  /** The path across the hub plate, when light goes through the junction (else hidden). */
  mid: Lane | null;
  /** Arc length spent inside the hub plate (the router). */
  hold: number;
  /** Sideways offset inside the lane: many strokes side by side, like traffic. */
  drift: number;
  total: number;
};

/** Seconds for light to fall to 1/e: the length of the exposure's memory. */
export const TAU = 5.5;
/** History drawn when the exposure is rebuilt in one pass (weights below ~2% are dropped). */
export const HISTORY = TAU * 3.5;
/** The moment the poster shows and the live view starts from, so the crossfade matches. */
export const POSTER_T = 30;
/** Floats per deposit instance: [x0 y0 x1 y1] [s0 s1 age0 age1] [channel intensity sigma dash period]. */
export const STRIDE = 12;

const HUB_DELAY = 0.16;
const STYLE = {
  task: { speed: 250, intensity: 1, sigma: 1, dash: 0 },
  status: { speed: 340, intensity: 0.6, sigma: 0.75, dash: 12 },
  event: { speed: 340, intensity: 0.6, sigma: 0.75, dash: 12 },
  result: { speed: 210, intensity: 1.7, sigma: 1.25, dash: 0 },
} as const;

export function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ORCH_WEIGHT: Record<string, number> = { me: 0.42, "claude-web-1": 0.24, "codex-2": 0.2, "claude-web-2": 0.14 };
const PROGRESS = ["progress · reading files", "progress · cargo test", "progress · editing", "progress · npm run lint", "progress · writing summary"];

/** Tasks scripted to line up with the hero log under the band (task.7f3a, task.c91e). */
const SCRIPTED = [
  { at: POSTER_T - 4.2, from: "me", to: "claude-1", id: "7f3a2c1e" },
  { at: POSTER_T - 2.4, from: "me", to: "codex-1", id: "c91e04b7" },
];

export function createTraffic(layout: Layout, seed = 7, opts: { through?: boolean } = {}) {
  const rand = rng(seed);
  const byId = new Map(layout.agents.map((a) => [a.id, a]));
  const orchestrators = layout.agents.filter((a) => a.role === "orchestrator");
  const workers = layout.agents.filter((a) => a.role === "worker");
  const busy = new Map<string, number>();
  const pending: Msg[] = [];
  let clock = 0.4;
  let scripted = 0;

  const message = (task: string, from: Agent, to: Agent, kind: Kind, detail: string, t0: number): Msg => {
    const st = STYLE[kind];
    const speed = st.speed * layout.speed * (0.92 + rand() * 0.16);
    const a = layout.lanes[from.id].toHub;
    const b = layout.lanes[to.id].fromHub;
    const mid = opts.through ? crossing(layout, from.id, to.id) : null;
    const hold = mid ? mid.length : HUB_DELAY * speed;
    const drift = (rand() - 0.5) * 2 * layout.drift;
    return { task, from: from.id, to: to.id, kind, detail, channel: kind === "task" ? 0 : 1, t0, speed, a, b, mid, hold, drift, total: a.length + hold + b.length };
  };
  const arrival = (m: Msg) => m.t0 + m.total / m.speed;

  const pickOrchestrator = () => {
    let r = rand();
    for (const o of orchestrators) if ((r -= ORCH_WEIGHT[o.id] ?? 0.1) <= 0) return o;
    return orchestrators[0];
  };

  function plan(t: number, orch: Agent, worker: Agent, id: string) {
    const task = `task.${id}`;
    const out = message(task, orch, worker, "task", `prompt, #${task}`, t);
    const msgs = [out];
    let at = arrival(out) + 0.1;
    msgs.push(message(task, worker, orch, "status", "working", at));
    at += 0.28;
    msgs.push(message(task, worker, orch, "event", "started", at));
    const n = 1 + Math.floor(rand() * 3);
    for (let i = 0; i < n; i++) {
      at += 0.9 + rand() * 1.5;
      msgs.push(message(task, worker, orch, "event", PROGRESS[Math.floor(rand() * PROGRESS.length)], at));
    }
    at += 0.8 + rand() * 1.4;
    const result = message(task, worker, orch, "result", "done · 1 result", at);
    msgs.push(result);
    busy.set(worker.id, arrival(result));
    pending.push(...msgs);
  }

  const hex = () => Math.floor(rand() * 0xffffffff).toString(16).padStart(8, "0");

  /** Plan every task that starts before `t`. Deterministic: frame timing never touches the RNG. */
  function advance(t: number) {
    while (clock <= t) {
      const s = SCRIPTED[scripted];
      if (s && s.at <= clock) {
        scripted++;
        plan(s.at, byId.get(s.from)!, byId.get(s.to)!, s.id);
        continue;
      }
      const idle = workers.filter((w) => (busy.get(w.id) ?? -1) < clock && !SCRIPTED.slice(scripted).some((x) => x.to === w.id && x.at - clock < 14));
      if (idle.length) plan(clock, pickOrchestrator(), idle[Math.floor(rand() * idle.length)], hex());
      clock += 0.5 + rand() * 0.55;
    }
  }

  /** Messages whose light can still matter at time `t` (started, and not long finished). */
  function live(t: number): Msg[] {
    advance(t);
    for (let i = pending.length - 1; i >= 0; i--) if (arrival(pending[i]) < t - HISTORY) pending.splice(i, 1);
    return pending.filter((m) => m.t0 <= t);
  }

  return { live, arrival, advance, pending: () => pending as readonly Msg[] };
}

// ---- deposits ----

/** Index of the polyline segment containing arc length `s`. */
function seg(l: Lane, s: number) {
  let lo = 0, hi = l.cum.length - 1;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (l.cum[mid] <= s) lo = mid;
    else hi = mid;
  }
  return lo;
}

function at(l: Lane, i: number, s: number): [number, number] {
  const span = l.cum[i + 1] - l.cum[i] || 1;
  const u = (s - l.cum[i]) / span;
  return [l.pts[i * 2] + (l.pts[i * 2 + 2] - l.pts[i * 2]) * u, l.pts[i * 2 + 1] + (l.pts[i * 2 + 3] - l.pts[i * 2 + 1]) * u];
}

export class Deposits {
  readonly data: Float32Array<ArrayBuffer>;
  count = 0;
  constructor(readonly capacity: number) {
    this.data = new Float32Array(capacity * STRIDE);
  }
  reset() {
    this.count = 0;
  }
  push(v: number[]) {
    if (this.count >= this.capacity) return;
    this.data.set(v, this.count++ * STRIDE);
  }
}

/** Light a message laid down between `ta` and `tb`, each piece weighted by its age at `tb`. */
export function emit(m: Msg, ta: number, tb: number, out: Deposits, layout: Layout) {
  const sA = Math.max(0, (ta - m.t0) * m.speed);
  const sB = Math.min(m.total, (tb - m.t0) * m.speed);
  const st = STYLE[m.kind];
  const sigma = st.sigma * layout.sigma;
  const piece = (l: Lane, base: number) => {
    const u0 = Math.max(0, sA - base);
    const u1 = Math.min(l.length, sB - base);
    if (u1 <= u0) return;
    const age = (u: number) => tb - (m.t0 + (base + u) / m.speed);
    for (let i = seg(l, u0); i < l.cum.length - 1 && l.cum[i] < u1; i++) {
      const a = Math.max(u0, l.cum[i]);
      const b = Math.min(u1, l.cum[i + 1]);
      if (b <= a) continue;
      const p = at(l, i, a);
      const q = at(l, i, b);
      // Drift sideways (left of travel) along this lane piece's own normal.
      const dx = l.pts[i * 2 + 2] - l.pts[i * 2];
      const dy = l.pts[i * 2 + 3] - l.pts[i * 2 + 1];
      const k = m.drift / (Math.hypot(dx, dy) || 1);
      out.push([p[0] + dy * k, p[1] - dx * k, q[0] + dy * k, q[1] - dx * k, a, b, age(a), age(b), m.channel, st.intensity, sigma, st.dash]);
    }
  };
  if (sB > sA) {
    piece(m.a, 0);
    if (m.mid) piece(m.mid, m.a.length);
    piece(m.b, m.a.length + m.hold);
  }
}

/** Everything still visible at `t`, in one batch (the poster, and the first live frame). */
export function exposure(traffic: ReturnType<typeof createTraffic>, layout: Layout, t: number, out: Deposits) {
  out.reset();
  for (const m of traffic.live(t)) emit(m, t - HISTORY, t, out, layout);
  return out;
}

// ---- the hub lamp ----

export const LAMP_EVENTS = 12;
const LAMP_WEIGHT: Record<Kind, number> = { task: 0.8, status: 0.35, event: 0.35, result: 1 };

/**
 * What the hub shows at time `t`: each envelope registers where it enters the
 * plate (a short rim flash in its own colour), and a slow glow follows recent
 * throughput. Events: [x, y, age, weight], weight positive for sent (teal) and
 * negative for received (orange); unused slots have a large age.
 */
export function hubLamp(msgs: readonly Msg[], t: number) {
  let glow = 0;
  let sent = 0;
  const events: [number, number, number, number][] = [];
  for (const m of msgs) {
    const age = t - (m.t0 + m.a.length / m.speed);
    if (age < 0 || age > 12) continue;
    const w = LAMP_WEIGHT[m.kind];
    const g = w * Math.exp(-age / 2.5);
    glow += g;
    if (m.channel === 0) sent += g;
    if (age < 1.5) events.push([m.a.pts[m.a.pts.length - 2], m.a.pts[m.a.pts.length - 1], age, m.channel === 0 ? w : -w]);
  }
  events.sort((x, y) => x[2] - y[2]);
  events.length = Math.min(events.length, LAMP_EVENTS);
  while (events.length < LAMP_EVENTS) events.push([0, 0, 99, 0]);
  return { glow: 1 - Math.exp(-glow / 8), share: glow > 0 ? 1 - sent / glow : 0.5, events };
}
