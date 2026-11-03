// The ASCII field: the whole stage is a still grid of IBM Plex Mono cells
// whose text is the hub's own traffic (subjects, channels, envelopes). The
// long exposure only decides how brightly each cell is lit and in which
// colour. Rows near a machine's trunk carry that machine's agents' messages,
// so light falls on the part of the log it belongs to. Shared by the live
// shader (ascii.wgsl mirrors cellLook/glyph rules) and the SVG poster.
import type { Layout, Tune } from "./layout";
import { createTraffic, POSTER_T, type Msg } from "./schedule";

/** Columns × rows, anchored to the viewBox so the poster and live grids coincide (~8×14 CSS px at full width). */
export const GRID = { wide: [136, 29], tall: [44, 31] } as const;
/** Lanes a character row apart, so sent and received light land in different rows. */
export const TUNE: Record<"wide" | "tall", Tune> = { wide: { gap: 8, drift: 1.2 }, tall: { gap: 4.5, drift: 0.8 } };
/** Glyph size relative to the row height, and baseline offset (× font size) below the cell middle. */
export const FONT = { scale: 0.78, baseline: 0.3 };
/** Dim field: lane-muted at `base` alpha. Light saturates with `gain`; lit alpha rises as (a × scale)^power. */
export const LOOK = { base: 0.13, gain: 0.75, scale: 1.25, power: 1.1 };
/** Spaces between words step up this density ramp as light grows, so a lit trail never breaks. */
export const RAMP = { chars: [".", ":", "-", "="], at: [0.1, 0.28, 0.48, 0.7] };

const PROMPTS = [
  "Fix the failing router test",
  "Add a changelog entry",
  "Review the presence handler",
  "Write tests for the merge gate",
  "Update the docs index",
  "Profile hub-server startup",
  "Bump async-nats",
];
const RESULTS = ["1 file changed, tests pass", "2 files changed", "entry added", "no issues found", "PR ready for review"];

/** One log record per message, payload first (payloads are free-form JSON; the reply contract's fields are real). */
export function record(m: Msg, i: number): string {
  const id = m.task.slice(5);
  switch (m.kind) {
    case "task":
      return `hub.pub.${m.from}.${m.task} -> channel.inbox.${m.to} {"prompt":"${PROMPTS[i % PROMPTS.length]}","task_channel":"${m.task}"}`;
    case "status":
      return `channel.${m.task} {"status":"working","task_id":"${id}"}`;
    case "event":
      return m.detail === "started"
        ? `channel.${m.task} {"event_type":"started","data":{}}`
        : `hub.pub.${m.from}.${m.task} {"event_type":"progress","data":{"step":"${m.detail.replace("progress · ", "")}"}}`;
    case "result":
      return `channel.${m.task} {"status":"done","task_id":"${id}","result":"${RESULTS[i % RESULTS.length]}","error":null}  hub.presence {"identity":"${m.from}"}`;
  }
}

/** The field's text, one string of exactly `cols` characters per row. Deterministic. */
export function fieldRows(layout: Layout, cols: number, rows: number): string[] {
  const traffic = createTraffic(layout, 7, { through: true });
  traffic.advance(POSTER_T);
  const msgs = [...traffic.pending()].sort((a, b) => a.t0 - b.t0);
  const recs = msgs.map((m, i) => ({ m, text: record(m, i) }));
  const cellH = layout.height / rows;
  return Array.from({ length: rows }, (_, r) => {
    // Agents whose lanes pass through this row's band (outside the hub).
    const y0 = (r - 1) * cellH;
    const y1 = (r + 2) * cellH;
    const near = new Set<string>();
    for (const a of layout.agents) {
      const p = layout.lanes[a.id].toHub.pts;
      for (let i = 1; i < p.length; i += 2) if (p[i] >= y0 && p[i] <= y1) near.add(a.id);
    }
    const mine = recs.filter((x) => near.has(x.m.from) || near.has(x.m.to));
    const pool = mine.length > 2 ? mine : recs;
    let line = "";
    for (let k = (r * 7) % pool.length; line.length < cols * 2; k = (k + 1) % pool.length) line += pool[k].text + "  ";
    // Rows start a little way into a record, never so far that narrow (phone) rows lose its subject.
    const skip = (r * 13) % Math.max(1, Math.floor(cols / 4));
    return line.slice(skip, skip + cols);
  });
}

/** Every glyph the field can show (text + ramp + space) and the per-cell glyph index. */
export function glyphTable(rows: string[]) {
  const chars = [" ", ...RAMP.chars];
  for (const row of rows) for (const ch of row) if (!chars.includes(ch)) chars.push(ch);
  const index = new Uint8Array(rows.length * rows[0].length);
  rows.forEach((row, r) => [...row].forEach((ch, c) => (index[r * row.length + c] = chars.indexOf(ch))));
  return { chars, index };
}

/** Light at a cell (already soft-saturated, 0..1 per channel) → coverage and received share. */
export function cellLook(sent: number, received: number) {
  const a = 1 - (1 - sent) * (1 - received);
  return { a, share: received / Math.max(sent + received, 1e-5) };
}

/** The glyph a cell shows: its own text, or a ramp step where a lit cell falls on a space. */
export function glyphAt(ch: string, a: number): string {
  if (ch !== " ") return ch;
  let g = " ";
  RAMP.at.forEach((t, i) => {
    if (a >= t) g = RAMP.chars[i];
  });
  return g;
}

/** Final alpha of a lit cell's glyph: dim texture at zero light, full at strong light. */
export const litAlpha = (a: number) => LOOK.base + (1 - LOOK.base) * Math.min(1, Math.pow(a * LOOK.scale, LOOK.power));
