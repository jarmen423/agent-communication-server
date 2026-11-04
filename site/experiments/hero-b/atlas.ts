// Glyph atlas for the ASCII layers: IBM Plex Mono (the site's --font-mono)
// drawn into canvas 2D at the live cell size, coverage only, uploaded as RGBA
// texels (value in R: universally supported, and what the mock adapter models).
import { FONT } from "./glyphs";

/** Glyph coverage atlas: `cols` slots per row, each `slot` texels, one byte per texel. */
export type Atlas = { data: Uint8Array<ArrayBuffer>; width: number; height: number; cols: number; slot: [number, number] };
export type AtlasMaker = (chars: string[], cell: [number, number], fontPx: number) => Atlas;

export const ATLAS_COLS = 16;

/** One byte per texel → RGBA texels with the value in R. */
export function rgba(src: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(src.length * 4);
  for (let i = 0; i < src.length; i++) out[i * 4] = src[i];
  return out;
}

/** Browser atlas: Plex Mono drawn into canvas 2D, coverage only. */
export function canvasAtlas(family: string): AtlasMaker {
  return (chars, cell, fontPx) => {
    const slot: [number, number] = [Math.ceil(cell[0]), Math.ceil(cell[1])];
    const width = slot[0] * ATLAS_COLS;
    const height = slot[1] * Math.ceil(chars.length / ATLAS_COLS);
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d")!;
    ctx.font = `400 ${fontPx}px ${family}`;
    ctx.fillStyle = "#fff";
    ctx.textAlign = "center";
    ctx.textBaseline = "alphabetic";
    chars.forEach((ch, i) => {
      const x = (i % ATLAS_COLS) * slot[0] + cell[0] / 2;
      const y = Math.floor(i / ATLAS_COLS) * slot[1] + cell[1] / 2 + fontPx * FONT.baseline;
      ctx.fillText(ch, x, y);
    });
    const rgba = ctx.getImageData(0, 0, width, height).data;
    const data = new Uint8Array(width * height);
    for (let i = 0; i < data.length; i++) data[i] = rgba[i * 4 + 3];
    return { data, width, height, cols: ATLAS_COLS, slot };
  };
}

/** The site's mono family from CSS, and Plex Mono loaded (best effort: a fallback face failing must not cost the hero). */
export async function monoFamily(el: Element) {
  const family = getComputedStyle(el).getPropertyValue("--font-mono").trim() || "monospace";
  await document.fonts.load(`400 12px ${family.split(",")[0]}`).catch(() => undefined);
  await document.fonts.ready;
  return family;
}
