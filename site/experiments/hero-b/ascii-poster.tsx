// The ASCII variant's poster: the same field as real SVG text, lit from the
// baked per-cell light (tools/bake.mjs → posters/ascii-*.json, read back from
// cells.wgsl at the poster moment), with the hub lamp's glow at that moment.
// Server-rendered, theme-aware (CSS variables), crisp at any DPR, no JS.
import { cellLook, fieldRows, FONT, glyphAt, GRID, litAlpha, LOOK } from "./field";
import type { Layout } from "./layout";
import { createTraffic, hubLamp, POSTER_T } from "./schedule";
import tallCells from "./posters/ascii-tall.json";
import wideCells from "./posters/ascii-wide.json";

type Run = { text: string; fill: string; opacity: number };

export function AsciiPoster({ layout, className }: { layout: Layout; className?: string }) {
  const [cols, rows] = layout.compact ? GRID.tall : GRID.wide;
  const baked = (layout.compact ? tallCells : wideCells).cells;
  const text = fieldRows(layout, cols, rows);
  const cellW = layout.width / cols;
  const cellH = layout.height / rows;
  const font = cellH * FONT.scale;
  const spacing = cellW - font * 0.6; // Plex Mono advance is 0.6 em
  const lamp = hubLamp(createTraffic(layout, 7, { through: true }).live(POSTER_T), POSTER_T);
  const { plate } = layout;

  const runsOf = (r: number) => {
    const runs: Run[] = [];
    for (let c = 0; c < cols; c++) {
      const hex = baked.slice((r * cols + c) * 2, (r * cols + c) * 2 + 2);
      const { a, share } = cellLook(parseInt(hex[0], 16) / 15, parseInt(hex[1], 16) / 15);
      const edge = Math.min(Math.min(c + 0.5, cols - c - 0.5) / 3, Math.min(r + 0.5, rows - r - 0.5) / 2);
      const base = LOOK.base * Math.min(1, Math.max(0, edge));
      const lit = a * 6 >= 0.5;
      const fill = lit ? (share > 0.5 ? "var(--lane-orange)" : "var(--lane-teal)") : "var(--lane-muted)";
      const opacity = Math.round((lit ? litAlpha(a) : base) * 20) / 20;
      const ch = glyphAt(text[r][c], a);
      const last = runs[runs.length - 1];
      if (last && last.fill === fill && last.opacity === opacity) last.text += ch;
      else runs.push({ text: ch, fill, opacity });
    }
    return runs;
  };

  return (
    <svg viewBox={`0 0 ${layout.width} ${layout.height}`} className={className} aria-hidden="true" preserveAspectRatio="xMidYMid meet">
      <defs>
        <radialGradient id={`hub-glow-${cols}`}>
          <stop offset="0.45" stopColor={lamp.share > 0.5 ? "var(--lane-orange)" : "var(--lane-teal)"} stopOpacity={lamp.glow * 0.33} />
          <stop offset="1" stopColor="var(--lane)" stopOpacity={0} />
        </radialGradient>
      </defs>
      <ellipse cx={plate.x + plate.w / 2} cy={plate.y + plate.h / 2} rx={plate.w * 0.95} ry={plate.h * 0.95} fill={`url(#hub-glow-${cols})`} />
      <g className="dg-mono" fontSize={font} letterSpacing={spacing} xmlSpace="preserve">
        {Array.from({ length: rows }, (_, r) => (
          <text key={r} x={spacing / 2} y={r * cellH + cellH / 2 + font * FONT.baseline}>
            {runsOf(r).map((run, i) =>
              run.opacity === 0 ? (
                <tspan key={i} fillOpacity={0}>{run.text}</tspan>
              ) : (
                <tspan key={i} fill={run.fill} fillOpacity={run.opacity}>{run.text}</tspan>
              ),
            )}
          </text>
        ))}
      </g>
    </svg>
  );
}
