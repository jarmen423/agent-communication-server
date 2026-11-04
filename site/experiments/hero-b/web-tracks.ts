// The web's message tracks, the way plain B draws them: each place slot is a
// machine whose agents' branches merge at a gate just outside the card, then
// run as one trunk into the hub plate face that looks at the slot. Every agent
// gets two lanes (left-hand traffic, like B): toward the hub and back out, so
// sent (teal) and received (orange) light run side by side. The traffic is B's
// message rhythm (a task out; status, started and progress ticks back; one
// result), planned only between places that stay on screen for the whole task.
import { offset, type Pt } from "@/components/hero/geometry";
import { centerline, crossing, lane, type Layout, type Machine } from "./layout";
import { rng } from "./prng";
import { HISTORY, STYLE, type Kind, type Msg } from "./schedule";
import type { WebAgent, WebLayout } from "./web-model";
import { CARD, placeSteady } from "./web-places";

/**
 * B's track widths and speeds, in the web's design units. The hero is
 * height-bound (cover over 1000 design units), so on a laptop the web draws at
 * about 0.9 css px per unit, close to B's stage: widths stay B's, a touch
 * bolder; runs are longer here, so messages travel a little faster.
 */
const TRACK = {
  wide: { sigma: 0.75, halo: 2.8, gap: 5, drift: 2.6, speed: 1.3, gate: 40 },
  tall: { sigma: 0.72, halo: 2.6, gap: 4, drift: 1.8, speed: 0.7, gate: 22 },
} as const;

/** Where a slot's trunk starts (the gate) and where it meets the plate (entry, with the face's outward normal). */
export function slotGeometry(w: WebLayout, i: number) {
  const slot = w.slots[i];
  const card = CARD[w.variant];
  const tr = TRACK[w.variant];
  const port: Pt = [slot.x, slot.y + card.first + card.row * ((card.rows - 1) / 2)];
  const dx = w.hub.cx - port[0];
  const axis: Pt = [Math.sign(dx) || 1, 0];
  const gate: Pt = [port[0] + axis[0] * tr.gate, port[1]];
  const half = w.plate.w / 2;
  const d = [port[0] - w.hub.cx, port[1] - w.hub.cy];
  const n = Math.max(Math.abs(d[0]), Math.abs(d[1])) || 1;
  const entry: Pt = [w.hub.cx + (d[0] / n) * half, w.hub.cy + (d[1] / n) * half];
  const edge: Pt = Math.abs(d[0]) >= Math.abs(d[1]) ? [Math.sign(d[0]), 0] : [0, Math.sign(d[1])];
  return { gate, axis, entry, edge };
}

const layouts = new WeakMap<WebLayout, Layout>();

/** A plain-B layout for the web's band: machines are slots, lanes run from every port to the hub. */
export function trackLayout(w: WebLayout): Layout {
  const hit = layouts.get(w);
  if (hit) return hit;
  const tr = TRACK[w.variant];
  const machines: Machine[] = w.slots.map((slot, i) => ({
    id: `slot-${i}`,
    role: "worker",
    agents: w.agents.filter((a) => a.slot === i).map((a) => ({ id: a.id, machine: `slot-${i}`, role: a.role, x: a.x, y: a.y })),
    ...slotGeometry(w, i),
    label: { x: slot.x, y: slot.y, anchor: "start" },
  }));
  const lanes: Layout["lanes"] = {};
  for (const m of machines) {
    for (const a of m.agents) {
      const c = centerline(m, a);
      lanes[a.id] = { toHub: lane(offset(c, tr.gap)), fromHub: lane(offset(c, -tr.gap).reverse()) };
    }
  }
  const out: Layout = {
    compact: w.variant === "tall",
    width: w.width,
    height: w.height,
    hub: w.hub,
    plate: w.plate,
    machines,
    agents: machines.flatMap((m) => m.agents),
    lanes,
    sigma: tr.sigma,
    halo: tr.halo,
    gap: tr.gap,
    drift: tr.drift,
    speed: tr.speed,
  };
  layouts.set(w, out);
  return out;
}

const r1 = (n: number) => Math.round(n * 10) / 10;
const pt = (p: Pt) => `${r1(p[0])} ${r1(p[1])}`;

/**
 * A place's rail as an SVG path: the same cubics centerline() samples (each
 * agent's branch into the gate, then the trunk into the plate), so a quiet
 * place still reads as wired to the hub, right between its two lanes.
 */
export function railPath(w: WebLayout, slot: number, rows: number) {
  const m = trackLayout(w).machines[slot];
  const { gate, axis, entry, edge } = m;
  const span = Math.hypot(entry[0] - gate[0], entry[1] - gate[1]);
  let d = `M${pt(gate)}C${pt([gate[0] + axis[0] * span * 0.42, gate[1] + axis[1] * span * 0.42])} ${pt([entry[0] + edge[0] * span * 0.42, entry[1] + edge[1] * span * 0.42])} ${pt(entry)}`;
  const ports = new Map<string, Pt>();
  for (const a of m.agents) ports.set(`${a.x},${a.y}`, [a.x, a.y]);
  for (const n of [...ports.values()].sort((p, q) => p[1] - q[1]).slice(0, rows)) {
    const along = (gate[0] - n[0]) * axis[0] + (gate[1] - n[1]) * axis[1];
    if (Math.hypot(gate[0] - n[0], gate[1] - n[1]) < 1) continue;
    d += `M${pt(n)}C${pt([n[0] + axis[0] * along * 0.55, n[1] + axis[1] * along * 0.55])} ${pt([n[0] + axis[0] * along * 0.75, n[1] + axis[1] * along * 0.75])} ${pt(gate)}`;
  }
  return d;
}

const PROGRESS = ["progress · reading files", "progress · cargo test", "progress · editing", "progress · npm run lint", "progress · writing summary"];
/** After a task's result lands, its light still glows; its places stay on screen this much longer. */
const AFTERGLOW = 8;

/** The web's traffic as B messages (light.ts draws them). Deterministic: frame timing never touches the RNG. */
export function createWebTraffic(w: WebLayout, seed = 7) {
  const t = trackLayout(w);
  const rand = rng(seed);
  const busy = new Map<string, number>();
  const pending: Msg[] = [];
  const every: readonly [number, number] = w.variant === "tall" ? [0.6, 1.1] : w.band === "P" ? [0.5, 1] : [0.3, 0.7];
  let clock = 0.4;

  const steady = (a: WebAgent, t0: number, t1: number) => placeSteady(a.slot, w.slots[a.slot], a.j, t0, t1);
  const message = (task: string, from: WebAgent, to: WebAgent, kind: Kind, detail: string, t0: number): Msg => {
    const speed = STYLE[kind].speed * t.speed * (0.92 + rand() * 0.16);
    const a = t.lanes[from.id].toHub;
    const b = t.lanes[to.id].fromHub;
    const mid = crossing(t, from.id, to.id);
    const drift = (rand() - 0.5) * 2 * t.drift;
    return { task, from: from.id, to: to.id, kind, detail, channel: kind === "task" ? 0 : 1, t0, speed, a, b, mid, hold: mid.length, drift, total: a.length + mid.length + b.length };
  };
  const arrival = (m: Msg) => m.t0 + m.total / m.speed;

  function plan(at: number): boolean {
    const from = w.agents.filter((a) => a.role === "orchestrator" && steady(a, at, at + 2));
    if (!from.length) return false;
    const orch = from[Math.floor(rand() * from.length)];
    // Always between two places: the light visibly leaves one and arrives at another.
    const to = w.agents.filter((a) => a.role === "worker" && a.slot !== orch.slot && (busy.get(a.id) ?? -1) < at && steady(a, at, at + 2));
    if (!to.length) return false;
    const worker = to[Math.floor(rand() * to.length)];
    const task = `task.${Math.floor(rand() * 0xffffffff).toString(16).padStart(8, "0")}`;
    const out = message(task, orch, worker, "task", `prompt, #${task}`, at);
    const msgs = [out];
    let next = arrival(out) + 0.1;
    msgs.push(message(task, worker, orch, "status", "working", next));
    next += 0.28;
    msgs.push(message(task, worker, orch, "event", "started", next));
    const n = 1 + Math.floor(rand() * 3);
    for (let i = 0; i < n; i++) {
      next += 0.9 + rand() * 1.5;
      msgs.push(message(task, worker, orch, "event", PROGRESS[Math.floor(rand() * PROGRESS.length)], next));
    }
    next += 0.8 + rand() * 1.4;
    const result = message(task, worker, orch, "result", "done · 1 result", next);
    msgs.push(result);
    const end = arrival(result) + AFTERGLOW;
    if (!steady(orch, at, end) || !steady(worker, at, end)) return false;
    busy.set(worker.id, arrival(result));
    pending.push(...msgs);
    return true;
  }

  function advance(until: number) {
    while (clock <= until) clock += plan(clock) ? every[0] + rand() * (every[1] - every[0]) : 0.4;
  }

  return {
    layout: t,
    advance,
    arrival,
    /** Messages whose light can still matter at `at`. */
    live(at: number): Msg[] {
      advance(at);
      for (let i = pending.length - 1; i >= 0; i--) if (arrival(pending[i]) < at - HISTORY) pending.splice(i, 1);
      return pending.filter((m) => m.t0 <= at);
    },
    pending: () => pending as readonly Msg[],
  };
}
