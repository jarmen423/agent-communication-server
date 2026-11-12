// The few web facts the page needs before any GPU code loads (kept tiny, so
// the stage's first-paint chunk does not pull in the model or the schedule):
// the aspect-band queries, the design-space sizes and the cover mapping.

/** Desktop aspect bands (P under 1.45, M to 1.7, L to 1.95, W beyond) and the phone layout. */
export type Band = "P" | "M" | "L" | "W" | "tall";

/**
 * Band queries on the viewport's aspect ratio. CSS shows a band's poster layers
 * with these exact strings and the live layer picks its layout with
 * matchMedia on them, so the poster and the first live frame always agree.
 */
export const BAND_QUERY = { M: "(min-aspect-ratio: 29/20)", L: "(min-aspect-ratio: 17/10)", W: "(min-aspect-ratio: 39/20)" } as const;
export const bandFor = (matches: (q: string) => boolean): "P" | "M" | "L" | "W" =>
  matches(BAND_QUERY.W) ? "W" : matches(BAND_QUERY.L) ? "L" : matches(BAND_QUERY.M) ? "M" : "P";

/** Design-space size per variant: desktop bands share 2400 × 1000, phones 400 × 860. */
export const DESIGN = { wide: { width: 2400, height: 1000 }, tall: { width: 400, height: 860 } } as const;

/** Cover ("slice") mapping of the design space onto a box of `w` × `h` (any unit). */
export function cover(l: { readonly width: number; readonly height: number }, w: number, h: number) {
  const k = Math.max(w / l.width, h / l.height);
  return { k, x: (w - l.width * k) / 2, y: (h - l.height * k) / 2 };
}
