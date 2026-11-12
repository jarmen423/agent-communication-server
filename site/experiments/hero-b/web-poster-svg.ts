// The ASCII web's poster as one static SVG document (tools/bake-web.mjs writes
// posters/web.svg). The page pulls it in with <use href="…#id">,
// so the heavy text never sits in the HTML or Next's page payload, the file is
// content-hashed and cached, and the glyphs still inherit the page's Plex Mono
// (use-element clones inherit from the <use>; an SVG <img> could not).
//   #field-wide, #field-tall   the quiet text fields (desktop grid, coarser phone grid)
//   #hub-<band>                the hub's neutral glow at the poster moment
//   #rail-<band>-<slot>-<j>    a place's rail to the hub, faded with its card
//   #card-<band>-<slot>-<j>    a place card (name, ports, agent names), faded by the page
// The tracks' light is a baked PNG per band (tools/bake-web.mjs).
// One document, one request: browsers fetch <use> targets even when hidden, so
// splitting by variant would not save bytes.
// Colours are literal: the hero is dark in both themes, and paint inside an
// external document cannot see the page's CSS variables.
import { FONT } from "./glyphs";
import { WEB_LOOK, webRows } from "./web-light";
import { groundFrame, WEB, WEB_POSTER_T, type Band, type WebLayout } from "./web-model";
import { CARD, PLACES } from "./web-places";
import { agentIcon, iconUse, ICON_DEFS, placeIcon } from "./web-icons";
import { createWebTraffic, railPath } from "./web-tracks";

/** Dark-island brand colours (tokens.css, dark): identical in both themes. */
const INK = { muted: "#a3a9b0", ink: "#eef0ea", line: "#384049", paper: "#101317" };

const r2 = (n: number) => Math.round(n * 100) / 100;
/** Makers' own logo colours; bake with HERO_ICONS=mono for the single-colour variant. */
const COLOURED = !(typeof process !== "undefined" && process.env?.HERO_ICONS === "mono");
/** IBM Plex Mono advance, em. */
const MONO_ADVANCE = 0.6;
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export function metrics(l: WebLayout) {
  const [cols, rows] = l.grid;
  const cw = l.width / cols;
  const ch = l.height / rows;
  const font = ch * FONT.scale;
  return { cols, rows, cw, ch, font, spacing: cw - font * 0.6, baseline: (r: number) => r * ch + ch / 2 + font * FONT.baseline };
}

function field(l: WebLayout, id: string) {
  const m = metrics(l);
  const rows = webRows(m.cols, m.rows)
    .map((row, r) => `<text x="${r2(m.spacing / 2)}" y="${r2(m.baseline(r))}">${esc(row)}</text>`)
    .join("\n");
  return `<g id="${id}" font-size="${r2(m.font)}" letter-spacing="${r2(m.spacing)}" xml:space="preserve" fill="${INK.muted}" fill-opacity="${WEB_LOOK.base}">\n${rows}\n</g>`;
}

/** The hub's neutral glow at the poster moment (the lamp's first live frame, without its rim flashes). */
function hub(l: WebLayout) {
  const { glow } = groundFrame(l, createWebTraffic(l).live(WEB_POSTER_T), WEB_POSTER_T);
  const id = `glow-${l.band}`;
  return (
    `<radialGradient id="${id}"><stop offset="0.4" stop-color="${INK.ink}" stop-opacity="${r2(glow * 0.3)}"/><stop offset="1" stop-color="${INK.ink}" stop-opacity="0"/></radialGradient>\n` +
    `<ellipse id="hub-${l.band}" cx="${l.hub.cx}" cy="${l.hub.cy}" rx="${r2(l.plate.w * 1.05)}" ry="${r2(l.plate.h * 1.05)}" fill="url(#${id})"/>`
  );
}

const BANDS: Band[] = ["P", "M", "L", "W", "tall"];

/** Place cards for a band: names on the side away from the hub, a knockout halo in the ground colour; an icon before each name. */
function cards(l: WebLayout) {
  const card = CARD[l.variant];
  const halo = `stroke="${INK.paper}" stroke-width="${l.variant === "tall" ? 5 : 6}" stroke-linejoin="round" paint-order="stroke"`;
  const r = l.variant === "tall" ? 3.5 : 4.5;
  const s = r2(card.name * 1.15); // agent icon size
  const ts = r2(card.title * 1.25); // place icon size
  const gap = 4;
  return l.slots
    .flatMap((slot, i) =>
      // Reaches have no card: their places are beyond the frame.
      (slot.reach ? [] : slot.places).map((pid, j) => {
        const start = slot.align === "start";
        const anchor = start ? "start" : "end";
        const rows = PLACES[pid].agents.slice(0, card.rows).map(([id, role], k) => {
          const y = slot.y + card.first + k * card.row;
          const tone = role === "orchestrator" ? INK.ink : INK.muted;
          // Port, name, then the maker's mark beyond the name (mono type: the name's width is known).
          const tx = r2(start ? slot.x + 11 : slot.x - 11);
          const w = id.length * MONO_ADVANCE * card.name;
          const ix = r2(start ? slot.x + 11 + w + gap : slot.x - 11 - w - gap - s);
          const iy = r2(y - s / 2);
          return (
            `<circle cx="${slot.x}" cy="${y}" r="${r}" fill="${INK.paper}" stroke="${tone}" stroke-width="1.5"/>` +
            `<text x="${tx}" y="${r2(y + card.name * 0.34)}" text-anchor="${anchor}" font-size="${card.name}" fill="${INK.muted}" ${halo}>${esc(id)}</text>` +
            iconUse(agentIcon(id), ix, iy, s, COLOURED ? INK.ink : tone, COLOURED)
          );
        });
        // The place's icon reads before its name on both sides (mono type: the name's width is known).
        const titleX = r2(start ? slot.x - 5 + ts + gap : slot.x + 5);
        const titleW = pid.length * MONO_ADVANCE * card.title;
        const px = r2(start ? slot.x - 5 : slot.x + 5 - titleW - gap - ts);
        const py = r2(slot.y - card.title * 0.36 - ts / 2);
        return `<g id="card-${l.band}-${i}-${j}"><text x="${titleX}" y="${slot.y}" text-anchor="${anchor}" font-size="${card.title}" fill="${INK.ink}" ${halo}>${esc(pid)}</text>${iconUse(placeIcon(pid), px, py, ts, INK.ink)}${rows.join("")}</g>`;
      }),
    )
    .join("\n");
}

/** Each place's rail to the hub: its own element, so the label guard (which hides cards near the copy) never sees it. */
function rails(l: WebLayout) {
  const card = CARD[l.variant];
  return l.slots
    .flatMap((slot, i) =>
      slot.places.map(
        (pid, j) =>
          `<path id="rail-${l.band}-${i}-${j}" d="${railPath(l, i, PLACES[pid].agents.slice(0, card.rows).length)}" fill="none" stroke="${INK.line}" stroke-width="${l.variant === "tall" ? 1 : 1.4}"/>`,
      ),
    )
    .join("\n");
}

/** The whole document. Deterministic: the drift test rebuilds it and compares. */
export function posterSvg() {
  return (
    // Never drawn directly (only <use>d); the nominal size is for Next's static-asset import.
    `<svg xmlns="http://www.w3.org/2000/svg" width="2400" height="1000" viewBox="0 0 2400 1000">\n${ICON_DEFS}\n` +
    `${field(WEB.M, "field-wide")}\n${field(WEB.tall, "field-tall")}\n${BANDS.map((b) => hub(WEB[b])).join("\n")}\n${BANDS.map((b) => rails(WEB[b])).join("\n")}\n${BANDS.map((b) => cards(WEB[b])).join("\n")}\n</svg>\n`
  );
}
