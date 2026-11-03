// The loop the signal box plays, as pure data: no DOM, no GPU. The SSR poster
// renders frameAt(POSTER_T) and the GPU layer starts at the same moment, so the
// crossfade from poster to live is invisible.
//
// 1. me delegates #task.7f3a to claude-1: the route sets lamp by lamp, the task
//    tile runs me → hub-server → claude-1, progress streams back, then exactly
//    one result returns and the route releases behind it.
// 2. A wave fans out to codex-1 and hermes-1 in parallel; both results return.
import type { Scene } from "@/components/hero/geometry";
import { at, buildTracks, type Dir, type Track } from "./tracks";

export const PERIOD = 16.5;
/** The poster frame: the wave in flight, progress streaming back, hermes-1's result on its way. */
export const POSTER_T = 12.95;

type Kind = "task" | "status" | "event" | "result";
type Envelope = { kind: Kind; route: string; depart: number; detail: string };
type Task = { route: string; worker: string; channel: string; set: number; envelopes: Envelope[] };

// Leg times (s) per route, the same on both stages; the tall one is just smaller.
const TILE_LEG: Record<string, number> = { a: 0.9, b1: 1.15, b2: 0.95, b3: 1.15 };
const PULSE_LEG: Record<string, number> = { a: 0.5, b1: 0.6, b2: 0.5, b3: 0.6 };
const TILE_DWELL = 0.2;
const PULSE_DWELL = 0.06;
const SWEEP_A = 0.22;
const SWEEP_B = 0.3;

const task = (route: string, worker: string, channel: string, set: number, send: number, replies: [Kind, number, string][]): Task => ({
  route,
  worker,
  channel,
  set,
  envelopes: [{ kind: "task", route, depart: send, detail: "message" }, ...replies.map(([kind, depart, detail]) => ({ kind, route, depart, detail }))],
});

// Event types are the reply contract's (refocus.md §6, docs/WORKER_BACKENDS.md).
export const TASKS: Task[] = [
  task("b1", "claude-1", "#task.7f3a", 0, 0.6, [
    ["status", 3.05, "working"],
    ["event", 3.3, "started"],
    ["event", 4.25, "progress"],
    ["event", 4.95, "progress"],
    ["result", 5.75, "done"],
  ]),
  task("b2", "codex-1", "#wave.e2b9.task.t1", 8.6, 9.2, [
    ["status", 11.45, "working"],
    ["event", 11.7, "started"],
    ["event", 12.55, "progress"],
    ["result", 13.0, "done"],
  ]),
  task("b3", "hermes-1", "#wave.e2b9.task.t2", 8.6, 9.5, [
    ["status", 11.95, "working"],
    ["event", 12.2, "started"],
    ["result", 12.45, "done"],
  ]),
];

export type Sprite = { kind: "tile" | "pulse"; dir: Dir; track: number; s: number; x: number; y: number };
export type FrameState = {
  /** Per track (buildTracks order): lit arc-length interval [lo, hi]; lo >= hi means dark. */
  lit: [number, number][];
  sprites: Sprite[];
  /** Hub-server lamp, 0..1, for envelopes passing in each direction. */
  hub: { sent: number; received: number };
  /** Third line of each wide node card. */
  states: Record<string, string>;
  /** Route channel labels that are lit. */
  routes: Record<string, boolean>;
};

export type Model = { scene: Scene; tracks: Track[]; trackOf: (route: string, dir: Dir) => Track };

export function buildModel(scene: Scene): Model {
  const tracks = buildTracks(scene);
  const trackOf = (route: string, dir: Dir) => tracks.find((t) => t.route === route && t.dir === dir)!;
  return { scene, tracks, trackOf };
}

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));
/** Tiles pull away and slow into a stop; a soft ease, not a bounce. */
const ease = (u: number) => u + (u * u * (3 - 2 * u) - u) * 0.6;

type Pos = { leg: 0 | 1; track: Track; s: number; enter: number; exit: number } | { leg: "hub"; enter: number; exit: number } | null;

/** Where envelope `e` is at time t: leg 0 (first route), in the hub, leg 1, or not on the board. */
function position(m: Model, e: Envelope, t: number): Pos {
  const tile = e.kind === "task" || e.kind === "result";
  const legs = tile ? TILE_LEG : PULSE_LEG;
  const dwell = tile ? TILE_DWELL : PULSE_DWELL;
  const out = e.kind === "task";
  const first = out ? "a" : e.route;
  const second = out ? e.route : "a";
  const enter = e.depart + legs[first];
  const exit = enter + dwell;
  const end = exit + legs[second];
  if (t < e.depart || t > end) return null;
  if (t >= enter && t < exit) return { leg: "hub", enter, exit };
  const leg = t < enter ? 0 : 1;
  const track = m.trackOf(leg === 0 ? first : second, out ? "sent" : "received");
  const u0 = leg === 0 ? (t - e.depart) / legs[first] : (t - exit) / legs[second];
  const u = tile ? ease(clamp01(u0)) : clamp01(u0);
  // Start and finish just past the track ends, hidden under a card or the hub plate.
  const hide = (tile ? (m.scene.width > 400 ? 17 : 13) : 6) + 1;
  const from = out ? -hide : track.length + hide;
  const to = out ? track.length + hide : -hide;
  return { leg, track, s: from + (to - from) * u, enter, exit };
}

const arrival = (m: Model, e: Envelope) => {
  const tile = e.kind === "task" || e.kind === "result";
  const legs = tile ? TILE_LEG : PULSE_LEG;
  const out = e.kind === "task";
  return e.depart + legs[out ? "a" : e.route] + (tile ? TILE_DWELL : PULSE_DWELL) + legs[out ? e.route : "a"];
};

/** Lit intervals for one task, keyed by track index. */
function taskLit(m: Model, k: Task, t: number): Map<number, [number, number]> {
  const out = new Map<number, [number, number]>();
  const result = k.envelopes.find((e) => e.kind === "result")!;
  const firstReply = k.envelopes.find((e) => e.kind !== "task")!;
  const done = arrival(m, result);
  if (t < k.set || t >= done) return out;
  const aS = m.trackOf("a", "sent");
  const bS = m.trackOf(k.route, "sent");
  const aR = m.trackOf("a", "received");
  const bR = m.trackOf(k.route, "received");

  // Sent: the route sets from me outward before the tile leaves.
  let aHi = clamp01((t - k.set) / SWEEP_A) * aS.length;
  let bHi = clamp01((t - k.set - SWEEP_A) / SWEEP_B) * bS.length;
  // Received: the first reply lights the track as it runs home.
  let bRec: [number, number] = [bR.length, bR.length];
  let aRec: [number, number] = [aR.length, aR.length];
  const fp = position(m, firstReply, t);
  if (t >= firstReply.depart) {
    if (fp && fp.leg === 0) bRec = [Math.max(0, fp.s), bR.length];
    else {
      bRec = [0, bR.length];
      if (fp && fp.leg === 1) aRec = [Math.max(0, fp.s), aR.length];
      else if (!fp || fp.leg !== "hub") aRec = [0, aR.length];
    }
  }
  // The one result releases the route behind it, section by section.
  const rp = position(m, result, t);
  if (rp) {
    if (rp.leg === 0) {
      const s = clamp01(rp.s / bR.length);
      bRec = [0, s * bR.length];
      bHi = Math.min(bHi, s * bS.length);
    } else {
      bRec = [0, 0];
      bHi = 0;
      if (rp.leg === 1) {
        const s = clamp01(rp.s / aR.length);
        aRec = [0, s * aR.length];
        aHi = Math.min(aHi, s * aS.length);
      }
    }
  }
  out.set(aS.k, [0, aHi]);
  out.set(bS.k, [0, bHi]);
  out.set(aR.k, aRec);
  out.set(bR.k, bRec);
  return out;
}

const STATE: Record<string, string> = { task: "task arriving", status: "working", result: "result sent" };

export function frameAt(m: Model, time: number): FrameState {
  const t = ((time % PERIOD) + PERIOD) % PERIOD;
  const lit: [number, number][] = m.tracks.map(() => [0, 0]);
  const sprites: Sprite[] = [];
  const hub = { sent: 0, received: 0 };
  const states: Record<string, string> = { me: "idle", "claude-1": "idle", "codex-1": "idle", "hermes-1": "idle" };
  const routes: Record<string, boolean> = {};
  let open = 0;

  for (const k of TASKS) {
    // Union of intervals on shared tracks (route a carries every task).
    for (const [i, [lo, hi]] of taskLit(m, k, t)) {
      if (hi <= lo) continue;
      const cur = lit[i];
      lit[i] = cur[1] <= cur[0] ? [lo, hi] : [Math.min(cur[0], lo), Math.max(cur[1], hi)];
    }
    const result = k.envelopes.find((e) => e.kind === "result")!;
    const done = arrival(m, result);
    if (t >= k.set && t < done) {
      open++;
      routes[k.route] = true;
    }

    for (const e of k.envelopes) {
      const p = position(m, e, t);
      if (p && p.leg !== "hub") {
        const [x, y] = at(p.track, p.s);
        const tile = e.kind === "task" || e.kind === "result";
        sprites.push({ kind: tile ? "tile" : "pulse", dir: p.track.dir, track: p.track.k, s: p.s, x, y });
      }
      if (p || (t >= e.depart && t < arrival(m, e) + 0.4)) {
        // Hub lamp: quick warm-up as the envelope enters, slower cool-down after it leaves.
        const tile = e.kind === "task" || e.kind === "result";
        const enter = e.depart + (tile ? TILE_LEG : PULSE_LEG)[e.kind === "task" ? "a" : e.route];
        const exit = enter + (tile ? TILE_DWELL : PULSE_DWELL);
        const f = (tile ? 1 : 0.45) * clamp01((t - enter + 0.06) / 0.08) * Math.exp(-Math.max(0, t - exit) / 0.2);
        if (e.kind === "task") hub.sent = Math.max(hub.sent, f);
        else hub.received = Math.max(hub.received, f);
      }
      // Worker state follows the envelopes it has sent or received.
      const ws = e.kind === "task" ? (p && p.leg !== 0 ? p.exit : Infinity) : e.depart;
      if (t >= ws && t < done) states[k.worker] = e.kind === "event" ? "working" : STATE[e.kind];
    }
  }
  if (open) states.me = `waiting on ${open} task${open > 1 ? "s" : ""}`;
  else {
    // Between deliveries: how many results the last round brought back.
    const last = Math.max(...TASKS.filter((k) => k.set <= t).map((k) => k.set));
    const n = TASKS.filter((k) => k.set === last).length;
    if (Number.isFinite(last)) states.me = `done · ${n} result${n > 1 ? "s" : ""}`;
  }
  for (const k of TASKS) {
    // A task that has arrived stays "working" until its result leaves.
    const task = k.envelopes[0];
    const arrived = arrival(m, task);
    const result = k.envelopes.find((e) => e.kind === "result")!;
    if (t >= arrived && t < result.depart) states[k.worker] = "working";
  }
  return { lit, sprites, hub, states, routes };
}
