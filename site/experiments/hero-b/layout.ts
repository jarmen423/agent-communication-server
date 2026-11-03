// Concept B, "long exposure": the junction. Orchestrator machines sit on one
// side of the hub and worker machines on the other. Each machine's agents fan
// into one shared trunk, and every trunk enters the hub plate. Each agent has
// two lanes, left-hand traffic like geometry.ts: teal (sent) always flows
// away from the orchestrators, orange (received) always flows back.
import { offset, type Pt, type Scene } from "@/components/hero/geometry";

export type Role = "orchestrator" | "worker";
export type Agent = { id: string; machine: string; role: Role; x: number; y: number; label?: "before" | "after" };
export type Machine = {
  id: string;
  role: Role;
  agents: Agent[];
  /** Where the agents' branches merge into the machine's trunk. */
  gate: Pt;
  /** Unit direction from the gate toward the hub. */
  axis: Pt;
  /** Where the trunk meets the hub plate, and the plate edge's outward normal there. */
  entry: Pt;
  edge: Pt;
  /** Machine name position (SVG text anchor point). */
  label: { x: number; y: number; anchor: "start" | "middle" | "end" };
};
/** A lane polyline with its cumulative arc length, in viewBox units. */
export type Lane = { pts: Float32Array; cum: Float32Array; length: number };
export type Layout = {
  compact: boolean;
  width: number;
  height: number;
  hub: { cx: number; cy: number; size: number };
  plate: { x: number; y: number; w: number; h: number; r: number };
  machines: Machine[];
  agents: Agent[];
  /** Per agent: the lane toward the hub and the lane back out to the agent. */
  lanes: Record<string, { toHub: Lane; fromHub: Lane }>;
  /** Trail widths (gaussian sigma, viewBox units), lane half-gap and in-lane drift. */
  sigma: number;
  halo: number;
  gap: number;
  drift: number;
  /** Message speeds scale with the stage (phones get calmer, shorter runs). */
  speed: number;
};

type MachineSpec = Omit<Machine, "agents"> & { ids: string[]; at: Pt; step: Pt; labels: Record<string, "before" | "after"> };

function machine(spec: MachineSpec): Machine {
  const agents = spec.ids.map((id, i) => ({
    id,
    machine: spec.id,
    role: spec.role,
    x: spec.at[0] + spec.step[0] * i,
    y: spec.at[1] + spec.step[1] * i,
    label: spec.labels[id],
  }));
  const { id, role, gate, axis, entry, edge, label } = spec;
  return { id, role, gate, axis, entry, edge, label, agents };
}

// ---- curves ----

const add = (a: Pt, b: Pt, k = 1): Pt => [a[0] + b[0] * k, a[1] + b[1] * k];
const dist = (a: Pt, b: Pt) => Math.hypot(b[0] - a[0], b[1] - a[1]);

function cubic(p0: Pt, p1: Pt, p2: Pt, p3: Pt, n: number): Pt[] {
  const out: Pt[] = [];
  for (let i = 0; i <= n; i++) {
    const t = i / n;
    const u = 1 - t;
    const a = u * u * u, b = 3 * u * u * t, c = 3 * u * t * t, d = t * t * t;
    out.push([a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]]);
  }
  return out;
}

/** Agent → gate (a short branch that meets the trunk at an angle) → hub plate. */
export function centerline(m: Machine, a: Agent): Pt[] {
  const n: Pt = [a.x, a.y];
  const along = (m.gate[0] - n[0]) * m.axis[0] + (m.gate[1] - n[1]) * m.axis[1];
  const branchLen = dist(n, m.gate);
  const branch = branchLen < 1 ? [n] : cubic(n, add(n, m.axis, along * 0.55), add(n, m.axis, along * 0.75), m.gate, Math.max(4, Math.ceil(branchLen / 6)));
  const span = dist(m.gate, m.entry);
  const trunk = cubic(m.gate, add(m.gate, m.axis, span * 0.42), add(m.entry, m.edge, span * 0.42), m.entry, Math.ceil(span / 7));
  return [...branch, ...trunk.slice(1)];
}

function lane(points: Pt[]): Lane {
  const pts = new Float32Array(points.length * 2);
  const cum = new Float32Array(points.length);
  points.forEach((p, i) => {
    pts[i * 2] = p[0];
    pts[i * 2 + 1] = p[1];
    if (i > 0) cum[i] = cum[i - 1] + dist(points[i - 1], p);
  });
  return { pts, cum, length: cum[cum.length - 1] };
}

function build(base: Omit<Layout, "agents" | "lanes">): Layout {
  const agents = base.machines.flatMap((m) => m.agents);
  const lanes: Layout["lanes"] = {};
  for (const m of base.machines) {
    for (const a of m.agents) {
      const c = centerline(m, a);
      // offset(+d) is left of travel (agent → hub); the way back is the other side, reversed.
      lanes[a.id] = { toHub: lane(offset(c, base.gap)), fromHub: lane(offset(c, -base.gap).reverse()) };
    }
  }
  return { ...base, agents, lanes };
}

// ---- the two stages (hub positions match components/hero/geometry.ts) ----

const H = (cx: number, cy: number, size: number, r: number) => {
  const half = size * 0.78;
  return { hub: { cx, cy, size }, plate: { x: cx - half, y: cy - half, w: half * 2, h: half * 2, r } };
};

/** Desktop, 1200 × 440: orchestrators left, workers right. */
export function wideLayout(scene: Pick<Scene, "width" | "height" | "hub"> = { width: 1200, height: 440, hub: { cx: 600, cy: 220, size: 72 } }): Layout {
  const { hub, plate } = H(scene.hub.cx, scene.hub.cy, scene.hub.size, 20);
  const L = plate.x;
  const R = plate.x + plate.w;
  const right: Pt = [1, 0];
  const left: Pt = [-1, 0];
  const machines = [
    machine({ id: "claude.ai", role: "orchestrator", ids: ["claude-web-1", "claude-web-2"], at: [150, 92], step: [0, 20], labels: { "claude-web-1": "before" },
      gate: [222, 102], axis: right, entry: [L, 206], edge: left, label: { x: 142, y: 70, anchor: "end" } }),
    machine({ id: "laptop", role: "orchestrator", ids: ["me", "codex-2"], at: [150, 318], step: [0, 20], labels: { me: "before", "codex-2": "before" },
      gate: [222, 328], axis: right, entry: [L, 234], edge: left, label: { x: 142, y: 296, anchor: "end" } }),
    machine({ id: "workstation", role: "worker", ids: ["claude-1", "claude-3", "claude-4", "cursor-1", "grok-1"], at: [1018, 44], step: [0, 18], labels: { "claude-1": "after" },
      gate: [958, 80], axis: left, entry: [R, 194], edge: right, label: { x: 1010, y: 26, anchor: "start" } }),
    machine({ id: "build-box", role: "worker", ids: ["codex-1", "codex-3", "codex-4", "opencode-1", "kilo-1"], at: [1104, 172], step: [0, 18], labels: { "codex-1": "after" },
      gate: [1042, 208], axis: left, entry: [R, 212], edge: right, label: { x: 1096, y: 154, anchor: "start" } }),
    machine({ id: "home-server", role: "worker", ids: ["hermes-1", "hermes-2", "hermes-3", "agy-1"], at: [1018, 306], step: [0, 18], labels: { "hermes-1": "after" },
      gate: [958, 333], axis: left, entry: [R, 230], edge: right, label: { x: 1010, y: 288, anchor: "start" } }),
    machine({ id: "ci-runner", role: "worker", ids: ["ci-1", "ci-2", "ci-3", "ci-4", "ci-5", "ci-6"], at: [714, 410], step: [18, 0], labels: {},
      gate: [759, 362], axis: [0, -1], entry: [R, 248], edge: right, label: { x: 818, y: 414, anchor: "start" } }),
  ];
  return build({ compact: false, width: scene.width, height: scene.height, hub, plate, machines, sigma: 0.65, halo: 2.4, gap: 4.4, drift: 2.4, speed: 1 });
}

/** Phones, 360 × 440: orchestrators on top, workers along the bottom. */
export function tallLayout(scene: Pick<Scene, "width" | "height" | "hub"> = { width: 360, height: 440, hub: { cx: 180, cy: 226, size: 60 } }): Layout {
  const { hub, plate } = H(scene.hub.cx, scene.hub.cy, scene.hub.size, 14);
  const T = plate.y;
  const B = plate.y + plate.h;
  const down: Pt = [0, 1];
  const up: Pt = [0, -1];
  const row = (id: string, ids: string[], cx: number, entryX: number, labels: Record<string, "before" | "after"> = {}) =>
    machine({ id, role: "worker", ids, at: [cx - ((ids.length - 1) * 13) / 2, 372], step: [13, 0], labels,
      gate: [cx, 332], axis: up, entry: [entryX, B], edge: down, label: { x: cx, y: 397, anchor: "middle" } });
  const machines = [
    machine({ id: "claude.ai", role: "orchestrator", ids: ["claude-web-1", "claude-web-2"], at: [52, 52], step: [16, 0], labels: {},
      gate: [60, 92], axis: down, entry: [166, T], edge: up, label: { x: 60, y: 30, anchor: "middle" } }),
    machine({ id: "laptop", role: "orchestrator", ids: ["me", "codex-2"], at: [292, 52], step: [16, 0], labels: {},
      gate: [300, 92], axis: down, entry: [194, T], edge: up, label: { x: 300, y: 30, anchor: "middle" } }),
    row("workstation", ["claude-1", "claude-3", "cursor-1", "grok-1"], 48, 146),
    row("build-box", ["codex-1", "codex-3", "opencode-1", "kilo-1"], 136, 170),
    row("home-server", ["hermes-1", "hermes-2", "hermes-3", "agy-1"], 224, 192),
    row("ci-runner", ["ci-1", "ci-2", "ci-3", "ci-4"], 312, 214),
  ];
  return build({ compact: true, width: scene.width, height: scene.height, hub, plate, machines, sigma: 0.55, halo: 2, gap: 3.2, drift: 1.6, speed: 0.55 });
}

export const layoutFor = (scene: Pick<Scene, "compact" | "width" | "height" | "hub">): Layout =>
  scene.compact ? tallLayout(scene) : wideLayout(scene);
