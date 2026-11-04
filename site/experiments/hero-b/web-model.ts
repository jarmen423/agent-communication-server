// The ASCII web: the whole hero is a still field of protocol text, a quiet
// ground under plain B's long-exposure tracks (web-tracks.ts, web-trails.ts).
// Named places sit around hub-server and come and go (web-places.ts). The
// ground itself only answers the hub: a neutral glow that follows
// throughput, a rim flash where each envelope enters the plate, a faint ring
// out through the field for every envelope routed, and a ring where a
// visitor clicks. Everything here is analytic and deterministic, so the
// server poster and the live layer agree.
import type { Kind, Msg } from "./schedule";
import type { Band } from "./web-bands";
import { CARD, PLACES, SLOTS, type Role, type Slot } from "./web-places";

export { BAND_QUERY, bandFor, cover, type Band } from "./web-bands";
export type WebAgent = { id: string; role: Role; x: number; y: number; place: string; slot: number; j: number };
export type WebLayout = {
  band: Band;
  variant: "wide" | "tall";
  /**
   * Design space, cover-mapped onto the hero. Desktop is 2400 × 1000: up to an
   * aspect of 2.4 the full height stays visible, so the safe band between the
   * header and the log strip is stable in design units; only the visible width
   * changes with aspect, which is what the bands are for.
   */
  width: number;
  height: number;
  hub: { cx: number; cy: number; size: number };
  plate: { x: number; y: number; w: number; h: number; r: number };
  grid: readonly [number, number];
  agents: WebAgent[];
  /** Location slots around the hub (see web-places.ts). */
  slots: Slot[];
  /** Ripple speed scale (phones are calmer). */
  speed: number;
  /**
   * The copy block (+16 px) measured at this band's reference viewport (P 768 × 1024,
   * M 1440 × 900, W 1894 × 920, phone 390 × 844), design units: the poster's mask.
   * The fifth number is the headline's bottom: above it the tracks yield less
   * (HEAD in web-light.ts), so the reaches show behind the large type while the
   * paragraph keeps its full yield. The live layer measures the real ones.
   */
  copy: readonly [number, number, number, number, number];
};

const plateOf = (cx: number, cy: number, size: number, r: number) => {
  const half = size * 0.82;
  return { x: cx - half, y: cy - half, w: half * 2, h: half * 2, r };
};

/** Every agent of every place a band's slots can show, at its port (card rows run down from the title). */
function agentsOf(slots: Slot[], card: (typeof CARD)["wide" | "tall"]): WebAgent[] {
  return slots.flatMap((slot, i) =>
    slot.places.flatMap((pid, j) =>
      PLACES[pid].agents.slice(0, card.rows).map(([id, role], k) => ({ id, role, x: slot.x, y: slot.y + card.first + k * card.row, place: pid, slot: i, j })),
    ),
  );
}

const desktop = (band: "P" | "M" | "W", hub: [number, number, number], copy: WebLayout["copy"]): WebLayout => ({
  band,
  variant: "wide",
  width: 2400,
  height: 1000,
  hub: { cx: hub[0], cy: hub[1], size: hub[2] },
  plate: plateOf(hub[0], hub[1], hub[2], 24),
  grid: [300, 71],
  speed: 1,
  agents: agentsOf(SLOTS[band], CARD.wide),
  slots: SLOTS[band],
  copy,
});

export const WEB: Record<Band, WebLayout> = {
  // Near-portrait screens (tablets, squarish windows): copy on top, the hub and its places below.
  P: desktop("P", [1200, 730, 92], [840, 120, 1332, 558, 350]),
  // Laptops: copy upper left, the hub upper right (about 78% across, 43% down), places around it, three reaching off the left edge.
  M: desktop("M", [1640, 430, 108], [551, 133, 1442, 583, 424]),
  // Wide and short screens: the same, with one more place along the bottom (no cards left of the copy).
  W: desktop("W", [1780, 430, 110], [565, 130, 1437, 570, 415]),
  tall: {
    band: "tall",
    variant: "tall",
    width: 400,
    height: 860,
    hub: { cx: 200, cy: 610, size: 66 },
    plate: plateOf(200, 610, 66, 16),
    grid: [40, 50],
    speed: 0.5,
    agents: agentsOf(SLOTS.tall, CARD.tall),
    slots: SLOTS.tall,
    copy: [17, 80, 384, 495, 258],
  },
};

// ---- the frame state the shader reads (and the poster evaluates) ----

export const RIPPLES = 8;
const LAMP_EVENTS = 12;
const LAMP_WEIGHT: Record<Kind, number> = { task: 0.8, status: 0.35, event: 0.35, result: 1 };

export type WebFrame = {
  /** Hub rim flashes: [x, y, age, weight], weight + sent (teal), - received (orange). */
  events: [number, number, number, number][];
  /** Rings out from the hub through the field: [radius, amp, width, unused]. */
  ripples: [number, number, number, number][];
  /** Neutral lamp, 0..1, following recent throughput. */
  glow: number;
  /** Visitor clicks: [x, y, radius, amp], neutral rings (at most CLICKS). */
  clicks: [number, number, number, number][];
  /** The copy block the field yields around (design units): x0, y0, x1, y1, feather. */
  mask: [number, number, number, number, number];
};

/** The moment the web's poster shows and its live layer starts from. */
export const WEB_POSTER_T = 30;

export const CLICKS = 4;
/** Feather of the copy mask, design units (at least 120 CSS px down to a 760 px tall hero). */
export const FEATHER = 170;
const CLICK_LIFE = 1.5;

/** A visitor's click: one neutral ring out from the pointer, a little stronger than a hub ripple. */
export function clickRings(clicks: readonly { x: number; y: number; t: number }[], t: number): WebFrame["clicks"] {
  const out: WebFrame["clicks"] = clicks
    .filter((c) => t >= c.t && t - c.t < CLICK_LIFE)
    .slice(-CLICKS)
    .map((c) => [c.x, c.y, (t - c.t) * 520, Math.exp(-(t - c.t) / 0.5)]);
  while (out.length < CLICKS) out.push([0, 0, 0, 0]);
  return out;
}

/**
 * What the ground shows at `t` for the messages in flight: each envelope
 * registers where it enters the plate (a short rim flash in its own colour)
 * and sends one faint ring out through the field; a slow glow follows
 * throughput. Like B's hubLamp, at the web's scale.
 */
export function groundFrame(l: WebLayout, msgs: readonly Msg[], t: number, extra: { clicks?: WebFrame["clicks"]; copy?: readonly number[] } = {}): WebFrame {
  const events: WebFrame["events"] = [];
  const ripples: WebFrame["ripples"] = [];
  let glow = 0;
  for (const m of msgs) {
    const age = t - (m.t0 + m.a.length / m.speed);
    if (age < 0 || age > 12) continue;
    const w = LAMP_WEIGHT[m.kind];
    glow += w * Math.exp(-age / 2.5);
    if (age < 1.5) events.push([m.a.pts[m.a.pts.length - 2], m.a.pts[m.a.pts.length - 1], age, m.channel === 0 ? w : -w]);
    if (age < 3.5) ripples.push([age * 260 * Math.max(l.speed, 0.6), w * Math.exp(-age / 1.4), 26 + age * 10, 0]);
  }
  events.sort((a, b) => a[2] - b[2]);
  ripples.sort((a, b) => b[1] - a[1]);
  events.length = Math.min(events.length, LAMP_EVENTS);
  ripples.length = Math.min(ripples.length, RIPPLES);
  while (events.length < LAMP_EVENTS) events.push([0, 0, 99, 0]);
  while (ripples.length < RIPPLES) ripples.push([0, 0, 1, 0]);
  const c = extra.copy ?? l.copy;
  const clicks = extra.clicks ?? clickRings([], t);
  return { events, ripples, glow: 1 - Math.exp(-glow / 6), clicks, mask: [c[0], c[1], c[2], c[3], FEATHER] };
}
