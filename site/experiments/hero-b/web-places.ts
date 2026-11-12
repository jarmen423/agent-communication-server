// Where the web's agents live: named locations (a laptop, a website, a
// teammate, cloud boxes, offices, homes, CI), each with a mix of agent kinds
// to show the hub is agent-agnostic and spans environments. Every band has a
// few slots around the hub; a slot shows one location at a time and, where it
// has several, rotates between them (fade out, a short gap, fade in), so the
// set of places changes calmly over time. On laptops and wider, a few places also
// sit beyond the frame, up and to the left: only their tracks show, reaching
// out of the hub behind the headline and off the edge. Deterministic, like
// the rest.

export type Role = "orchestrator" | "worker";
export type Place = { id: string; agents: readonly (readonly [string, Role])[] };

const O: Role = "orchestrator";
const W: Role = "worker";

/** The pool. Agent IDs name their kind: claude (Claude Code), codex, hermes, cursor, grok, opencode, kilo, agy (Antigravity), ci (any CLI), *-web (chat apps over the remote MCP connector). */
export const PLACES: Record<string, Place> = Object.fromEntries(
  (
    [
      ["laptop-1", [["me", O], ["codex-2", W], ["claude-11", W], ["cursor-4", W]]],
      ["website-2", [["claude-web-1", O], ["chatgpt-web-1", O], ["grok-web-1", O], ["claude-web-2", O]]],
      ["teammate-2", [["claude-8", O], ["hermes-3", W], ["codex-11", W], ["opencode-5", W]]],
      ["teammate-4", [["codex-5", O], ["claude-6", W], ["kilo-4", W], ["agy-3", W]]],
      ["cloud-1", [["codex-1", W], ["opencode-1", W], ["kilo-1", W], ["claude-12", W], ["codex-12", W]]],
      ["cloud-2", [["claude-2", W], ["codex-3", W], ["agy-1", W], ["hermes-6", W], ["grok-4", W]]],
      ["cloud-3", [["codex-4", W], ["kilo-2", W], ["claude-13", W], ["opencode-6", W]]],
      ["cloud-4", [["claude-7", W], ["opencode-3", W], ["codex-13", W], ["kilo-5", W], ["agy-4", W]]],
      ["office-1", [["cursor-2", W], ["claude-5", W], ["codex-14", W], ["grok-5", W]]],
      ["office-2", [["grok-2", W], ["codex-6", W], ["cursor-5", W], ["claude-14", W], ["hermes-7", W]]],
      ["office-3", [["claude-1", W], ["cursor-1", W], ["grok-1", W], ["codex-15", W]]],
      ["home-1", [["hermes-1", W], ["agy-2", W], ["claude-15", W], ["opencode-7", W]]],
      ["home-2", [["hermes-2", W], ["opencode-2", W], ["codex-16", W], ["kilo-6", W], ["claude-16", W]]],
      ["home-3", [["claude-4", W], ["hermes-4", W], ["cursor-6", W], ["codex-17", W]]],
      ["ci-4", [["ci-1", W], ["ci-2", W], ["ci-3", W], ["ci-5", W]]],
      // Beyond the frame (reach slots): never labelled, always there.
      ["cloud-5", [["codex-7", W], ["claude-3", W], ["kilo-3", W]]],
      ["teammate-5", [["claude-9", O], ["codex-8", W]]],
      ["office-4", [["cursor-3", W], ["hermes-5", W]]],
    ] as const
  ).map(([id, agents]) => [id, { id, agents }]),
);

type P2 = readonly [number, number];
/**
 * A reach: the slot sits beyond the frame (no card; its ports are off screen)
 * and only its trunk shows: the cubic from the gate through `c1` and `c2` into
 * the plate's left face at `face` (-1 top, 1 bottom), design units. Drawn by
 * hand per band so each one fans out of the hub, crosses the headline on a
 * slant and leaves the left edge clear of the header.
 */
export type Reach = { face: number; gate: P2; c1: P2; c2: P2 };
/** A slot: title baseline at (x, y); ports run down from it; names on the `align` side. */
export type Slot = { x: number; y: number; align: "start" | "end"; places: readonly string[]; reach?: Reach };

/** Card metrics per variant (design units): every place shows up to `rows` agents, so each track's end forks four or five ways (three on phones). */
export const CARD = {
  wide: { title: 13, name: 12, row: 18, first: 20, rows: 5 },
  tall: { title: 12, name: 11, row: 15, first: 17, rows: 3 },
} as const;

const S = (x: number, y: number, align: Slot["align"], ...places: string[]): Slot => ({ x, y, align, places });
/** A reach slot: one place beyond the frame, its ports just behind the gate (design x < 0: off screen at every aspect). */
const R = (place: string, face: number, gate: P2, c1: P2, c2: P2): Slot => {
  const rows = Math.min(PLACES[place].agents.length, CARD.wide.rows);
  return { x: gate[0] - 80, y: gate[1] - CARD.wide.first - (CARD.wide.row * (rows - 1)) / 2, align: "end", places: [place], reach: { face, gate, c1, c2 } };
};

/** Three tracks out of the hub's upper left, fanning behind the headline and off the left edge (laptops, then wide screens). */
const REACH = {
  M: [
    R("cloud-5", -0.74, [-20, 110], [650, 110], [1250, 180]),
    R("teammate-5", -0.5, [-20, 150], [700, 165], [1150, 300]),
    R("office-4", -0.26, [-20, 260], [600, 250], [1150, 420]),
  ],
  // The hub sits further right here; the near controls follow it (and pass under office-3's card).
  W: [
    R("cloud-5", -0.74, [-20, 110], [650, 110], [1350, 190]),
    R("teammate-5", -0.5, [-20, 150], [700, 165], [1260, 300]),
    R("office-4", -0.26, [-20, 260], [600, 250], [1260, 420]),
  ],
};

/**
 * Slots per band. Left of the hub, names sit left of the ports (lines leave
 * toward the hub); right of it, on the right. Every band keeps a teammate on
 * screen at all times (teammate-2 never rotates out), next to laptop-1's `me`.
 */
export const SLOTS: Record<"P" | "M" | "L" | "W" | "tall", Slot[]> = {
  P: [
    S(850, 620, "start", "office-3", "cloud-1", "home-1"),
    S(850, 760, "start", "laptop-1"),
    S(1550, 620, "end", "teammate-2"),
    S(1550, 760, "end", "cloud-2", "website-2", "ci-4"),
  ],
  M: [
    S(1490, 135, "start", "office-3", "cloud-2"),
    S(1778, 226, "start", "cloud-1", "home-2"),
    S(1795, 425, "start", "home-1", "teammate-4"),
    S(1809, 618, "start", "ci-4", "office-1"),
    S(1625, 735, "end", "cloud-3", "home-3"),
    S(1340, 728, "end", "office-2", "cloud-4", "website-2"),
    S(1040, 745, "end", "teammate-2"),
    S(740, 735, "end", "laptop-1"),
    ...REACH.M,
  ],
  // 16:9-ish: M's places, the right-hand ones further off the hub.
  L: [
    S(1490, 139, "start", "office-3", "cloud-2"),
    S(1900, 160, "start", "cloud-1", "home-2"),
    S(1911, 425, "start", "home-1", "teammate-4"),
    S(1925, 636, "start", "ci-4", "office-1"),
    S(1625, 735, "end", "cloud-3", "home-3"),
    S(1340, 728, "end", "office-2", "cloud-4", "website-2"),
    S(1040, 745, "end", "teammate-2"),
    S(740, 735, "end", "laptop-1"),
    ...REACH.M,
  ],
  // No cards left of the copy (a card's track would cross the headline); only the reaches go there.
  W: [
    S(1560, 163, "start", "office-3", "cloud-2"),
    S(1936, 165, "start", "cloud-1", "home-2", "home-1"),
    S(2024, 420, "start", "teammate-4", "cloud-3"),
    S(2027, 648, "start", "office-2", "cloud-4", "ci-4"),
    S(1690, 715, "end", "teammate-2"),
    S(1385, 715, "end", "office-1", "home-3"),
    S(1080, 715, "end", "website-2"),
    S(760, 720, "end", "laptop-1"),
    ...REACH.W,
  ],
  tall: [
    S(36, 540, "start", "laptop-1"),
    S(364, 540, "end", "office-3", "home-1"),
    // teammate-2 stays: with laptop-1 always there, some orchestrator always has a worker on screen.
    S(36, 640, "start", "teammate-2"),
    S(364, 640, "end", "cloud-1", "office-1", "home-2"),
  ],
};

/** One rotation through a slot's places takes this long; each place holds an equal share. */
const PERIOD = 90;
const FADE = 1.2;
const smooth = (x: number) => (x <= 0 ? 0 : x >= 1 ? 1 : x * x * (3 - 2 * x));

/** When place `j` of a slot is shown, around time `t`: [start, end) of its current window. */
function windowAt(slotIndex: number, slot: Slot, j: number, t: number) {
  const n = slot.places.length;
  if (n === 1) return [-Infinity, Infinity] as const;
  const share = PERIOD / n;
  const phase = slotIndex * 7.3;
  const k = Math.floor((t - phase - j * share) / PERIOD);
  const start = phase + k * PERIOD + j * share;
  return [start, start + share] as const;
}

/** How visible a slot's place is at `t` (0..1): fade in after the window opens, out before it closes, a gap between. */
export function placeAlpha(slotIndex: number, slot: Slot, j: number, t: number) {
  const [s, e] = windowAt(slotIndex, slot, j, t);
  if (!Number.isFinite(s)) return 1;
  return smooth((t - s - 0.8) / FADE) * (1 - smooth((t - (e - 0.2 - FADE)) / FADE));
}

/** True if place `j` of the slot is fully shown for the whole span [t0, t1]. */
export function placeSteady(slotIndex: number, slot: Slot, j: number, t0: number, t1: number) {
  const [s, e] = windowAt(slotIndex, slot, j, t0);
  return t0 >= s + 0.8 + FADE && t1 <= e - 0.2 - FADE;
}
