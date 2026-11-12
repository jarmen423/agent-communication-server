// Glyph-grid basics shared by the ASCII layers, kept free of any traffic code.

/** Glyph size relative to the row height, and baseline offset (× font size) below the cell middle. */
export const FONT = { scale: 0.78, baseline: 0.3 };

/** Every glyph the field can show (space, the ramp, then the text's own) and the per-cell glyph index. */
export function glyphTable(rows: string[], ramp: readonly string[]) {
  const chars = [" ", ...ramp];
  for (const row of rows) for (const ch of row) if (!chars.includes(ch)) chars.push(ch);
  const index = new Uint8Array(rows.length * rows[0].length);
  rows.forEach((row, r) => [...row].forEach((ch, c) => (index[r * row.length + c] = chars.indexOf(ch))));
  return { chars, index };
}
