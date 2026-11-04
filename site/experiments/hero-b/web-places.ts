// Where the web's agents live: named locations (a laptop, a website, a
// teammate, cloud boxes, offices, homes, CI), each with a mix of agent kinds
// to show the hub is agent-agnostic and spans environments. Every band has a
// few slots around the hub; a slot shows one location at a time and, where it
// has several, rotates between them (fade out, a short gap, fade in), so the
// set of places changes calmly over time. Deterministic, like the rest.

export type Role = "orchestrator" | "worker";
export type Place = { id: string; agents: readonly (readonly [string, Role])[] };

const O: Role = "orchestrator";
const W: Role = "worker";

/** The pool. Agent IDs name their kind: claude (Claude Code), codex, hermes, cursor, grok, opencode, kilo, agy (Antigravity), ci (any CLI), *-web (chat apps over the remote MCP connector). */
export const PLACES: Record<string, Place> = Object.fromEntries(
  (
    [
      ["laptop-1", [["me", O], ["codex-2", W]]],
      ["website-2", [["claude-web-1", O], ["chatgpt-web-1", O]]],
      ["teammate-2", [["claude-8", O], ["hermes-3", W]]],
      ["teammate-4", [["codex-5", O], ["claude-6", W]]],
      ["cloud-1", [["codex-1", W], ["opencode-1", W], ["kilo-1", W]]],
      ["cloud-2", [["claude-2", W], ["codex-3", W], ["agy-1", W]]],
      ["cloud-3", [["codex-4", W], ["kilo-2", W]]],
      ["cloud-4", [["claude-7", W], ["opencode-3", W]]],
      ["office-1", [["cursor-2", W], ["claude-5", W]]],
      ["office-2", [["grok-2", W], ["codex-6", W]]],
      ["office-3", [["claude-1", W], ["cursor-1", W], ["grok-1", W]]],
      ["home-1", [["hermes-1", W], ["agy-2", W]]],
      ["home-2", [["hermes-2", W], ["opencode-2", W]]],
      ["home-3", [["claude-4", W], ["hermes-4", W]]],
      ["ci-4", [["ci-1", W], ["ci-2", W]]],
    ] as const
  ).map(([id, agents]) => [id, { id, agents }]),
);

/** A slot: title baseline at (x, y); ports run down from it; names on the `align` side. */
export type Slot = { x: number; y: number; align: "start" | "end"; places: readonly string[] };

/** Card metrics per variant (design units). */
export const CARD = {
  wide: { title: 13, name: 12, row: 18, first: 20, rows: 3 },
  tall: { title: 12, name: 11, row: 16, first: 18, rows: 2 },
} as const;

const S = (x: number, y: number, align: Slot["align"], ...places: string[]): Slot => ({ x, y, align, places });

/** Slots per band. Left of the hub, names sit left of the ports (lines leave toward the hub); right of it, on the right. */
export const SLOTS: Record<"P" | "M" | "W" | "tall", Slot[]> = {
  P: [
    S(850, 620, "start", "office-3", "cloud-1"),
    S(850, 760, "start", "laptop-1"),
    S(1550, 620, "end", "website-2", "home-1"),
    S(1550, 760, "end", "cloud-2", "ci-4"),
  ],
  M: [
    S(1490, 165, "start", "office-3", "cloud-2"),
    S(1765, 235, "start", "cloud-1", "home-2"),
    S(1795, 425, "start", "home-1", "teammate-4"),
    S(1785, 615, "start", "ci-4", "office-1"),
    S(1625, 735, "end", "cloud-3", "home-3"),
    S(1340, 735, "end", "office-2", "cloud-4"),
    S(1040, 745, "end", "website-2", "teammate-2"),
    S(740, 735, "end", "laptop-1"),
  ],
  W: [
    S(345, 300, "start", "home-1", "ci-4"),
    S(345, 560, "start", "office-1", "home-3"),
    S(1560, 185, "start", "office-3", "cloud-2"),
    S(1880, 200, "start", "cloud-1", "home-2"),
    S(1965, 420, "start", "teammate-4", "cloud-3"),
    S(1955, 640, "start", "office-2", "cloud-4"),
    S(1690, 740, "end", "teammate-2"),
    S(1080, 740, "end", "website-2"),
    S(760, 730, "end", "laptop-1"),
  ],
  tall: [
    S(36, 540, "start", "laptop-1"),
    S(364, 540, "end", "office-3", "home-1"),
    S(36, 640, "start", "teammate-2", "home-2"),
    S(364, 640, "end", "cloud-1", "office-1"),
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
