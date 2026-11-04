// The ASCII web's poster as one static SVG document (tools/bake-web.mjs writes
// posters/web.svg). The page pulls it in with <use href="…#id">,
// so the heavy text never sits in the HTML or Next's page payload, the file is
// content-hashed and cached, and the glyphs still inherit the page's Plex Mono
// (use-element clones inherit from the <use>; an SVG <img> could not).
//   #field-wide, #field-tall   the quiet text fields (desktop grid, coarser phone grid)
//   #lit-<band>                the cells lit at the poster moment (the first live frame)
//   #card-<band>-<slot>-<j>    a place card (name, ports, agent names), faded by the page
// One document, one request: browsers fetch <use> targets even when hidden, so
// splitting by variant would not save bytes.
// Colours are literal: the hero is dark in both themes, and paint inside an
// external document cannot see the page's CSS variables.
import { FONT } from "./glyphs";
import { cellLight, cellLook, rampAt, WEB_LOOK, webRows } from "./web-light";
import { createWeb, frameAt, WEB, WEB_POSTER_T, type Band, type WebLayout } from "./web-model";
import { CARD, PLACES } from "./web-places";

/** Dark-island brand colours (tokens.css, dark): identical in both themes. */
const INK = { muted: "#a3a9b0", ink: "#eef0ea", teal: "#3cb8ab", orange: "#f5a742", paper: "#101317" };

const r2 = (n: number) => Math.round(n * 100) / 100;
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

function lit(l: WebLayout) {
  const m = metrics(l);
  const frame = frameAt(l, createWeb(l).live(WEB_POSTER_T), WEB_POSTER_T);
  const light = cellLight(l, frame);
  const text = webRows(m.cols, m.rows);
  const rows: string[] = [];
  for (let r = 0; r < m.rows; r++) {
    const runs: { c: number; text: string; fill: string; opacity: number }[] = [];
    for (let c = 0; c < m.cols; c++) {
      const i = (r * m.cols + c) * 4;
      const k = cellLook(light[i], light[i + 1], light[i + 2]);
      // Lifts under +0.025 alpha over the 6% texture are not visible in a still; dropping them halves the file.
      if (k.alpha < WEB_LOOK.base + 0.025 && k.hue < 0.5) continue;
      const fill = k.hue >= 0.5 ? (k.share > 0.5 ? INK.orange : INK.teal) : k.n > 0.35 ? INK.ink : INK.muted;
      const opacity = Math.round(k.alpha * 20) / 20;
      const g = rampAt(text[r][c], k.a);
      const last = runs.at(-1);
      if (last && last.c + last.text.length === c && last.fill === fill && last.opacity === opacity) last.text += g;
      else runs.push({ c, text: g, fill, opacity });
    }
    if (runs.length)
      rows.push(`<text y="${r2(m.baseline(r))}">${runs.map((x) => `<tspan x="${Math.round((x.c * m.cw + m.spacing / 2) * 10) / 10}" fill="${x.fill}" opacity="${x.opacity}">${esc(x.text)}</tspan>`).join("")}</text>`);
  }
  const id = `glow-${l.band}`;
  return (
    `<radialGradient id="${id}"><stop offset="0.4" stop-color="${INK.ink}" stop-opacity="${r2(frame.glow * 0.3)}"/><stop offset="1" stop-color="${INK.ink}" stop-opacity="0"/></radialGradient>\n` +
    `<g id="lit-${l.band}"><ellipse cx="${l.hub.cx}" cy="${l.hub.cy}" rx="${r2(l.plate.w * 1.05)}" ry="${r2(l.plate.h * 1.05)}" fill="url(#${id})"/>` +
    `<g font-size="${r2(m.font)}" letter-spacing="${r2(m.spacing)}" xml:space="preserve">\n${rows.join("\n")}\n</g></g>`
  );
}

const BANDS: Band[] = ["P", "M", "W", "tall"];

/** Place cards for a band: names on the side away from the hub, a knockout halo in the ground colour. */
function cards(l: WebLayout) {
  const card = CARD[l.variant];
  const halo = `stroke="${INK.paper}" stroke-width="${l.variant === "tall" ? 5 : 6}" stroke-linejoin="round" paint-order="stroke"`;
  const r = l.variant === "tall" ? 3.5 : 4.5;
  return l.slots
    .flatMap((slot, i) =>
      slot.places.map((pid, j) => {
        const start = slot.align === "start";
        const anchor = start ? "start" : "end";
        const rows = PLACES[pid].agents.slice(0, card.rows).map(([id, role], k) => {
          const y = slot.y + card.first + k * card.row;
          return (
            `<circle cx="${slot.x}" cy="${y}" r="${r}" fill="${INK.paper}" stroke="${role === "orchestrator" ? INK.ink : INK.muted}" stroke-width="1.5"/>` +
            `<text x="${start ? slot.x + 11 : slot.x - 11}" y="${r2(y + card.name * 0.34)}" text-anchor="${anchor}" font-size="${card.name}" fill="${INK.muted}" ${halo}>${esc(id)}</text>`
          );
        });
        return `<g id="card-${l.band}-${i}-${j}"><text x="${start ? slot.x - 5 : slot.x + 5}" y="${slot.y}" text-anchor="${anchor}" font-size="${card.title}" fill="${INK.ink}" ${halo}>${esc(pid)}</text>${rows.join("")}</g>`;
      }),
    )
    .join("\n");
}

/** The whole document. Deterministic: the drift test rebuilds it and compares. */
export function posterSvg() {
  return (
    // Never drawn directly (only <use>d); the nominal size is for Next's static-asset import.
    `<svg xmlns="http://www.w3.org/2000/svg" width="2400" height="1000" viewBox="0 0 2400 1000">\n` +
    `${field(WEB.M, "field-wide")}\n${field(WEB.tall, "field-tall")}\n${BANDS.map((b) => lit(WEB[b])).join("\n")}\n${BANDS.map((b) => cards(WEB[b])).join("\n")}\n</svg>\n`
  );
}
