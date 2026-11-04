// Server-rendered SVG for the ASCII web, in the same design space as the live
// canvas and cover-mapped the same way (preserveAspectRatio "slice"):
//  - WebField: the quiet text field, shared by every band of a variant.
//  - WebLit: only the cells lit at the poster moment (the first live frame),
//    per band, lit by cellLight() with that band's nominal copy mask.
//  - WebOverlay: the hub (glass plate and the lens of the mark), agent ports,
//    a few haloed names and direction in words, per band.
import { lensPath } from "@/components/hero/geometry";
import { FONT } from "./field";
import { POSTER_T } from "./schedule";
import { cellLight, cellLook, rampAt, WEB_LOOK, webRows } from "./web-light";
import { createWeb, frameAt, WEB, type WebLayout } from "./web-model";

const box = (l: WebLayout) => `0 0 ${l.width} ${l.height}`;

function metrics(l: WebLayout) {
  const [cols, rows] = l.grid;
  const cw = l.width / cols;
  const ch = l.height / rows;
  const font = ch * FONT.scale;
  return { cols, rows, cw, ch, font, spacing: cw - font * 0.6, baseline: (r: number) => r * ch + ch / 2 + font * FONT.baseline };
}

/** The quiet field: every row in one run, lane-muted at the base alpha. */
export function WebField({ variant, className }: { variant: "wide" | "tall"; className?: string }) {
  const l = variant === "wide" ? WEB.M : WEB.tall;
  const m = metrics(l);
  const text = webRows(m.cols, m.rows);
  return (
    <svg viewBox={box(l)} className={className} aria-hidden="true" preserveAspectRatio="xMidYMid slice">
      <g className="dg-mono" fontSize={m.font} letterSpacing={m.spacing} xmlSpace="preserve" fill="var(--lane-muted)" fillOpacity={WEB_LOOK.base}>
        {text.map((row, r) => (
          <text key={r} x={m.spacing / 2} y={m.baseline(r)}>
            {row}
          </text>
        ))}
      </g>
    </svg>
  );
}

type Run = { c: number; text: string; fill: string; opacity: number };

/** The cells lit at the poster moment, drawn over the quiet field at the same grid positions. */
export function WebLit({ layout: l, className }: { layout: WebLayout; className?: string }) {
  const m = metrics(l);
  const frame = frameAt(l, createWeb(l).live(POSTER_T), POSTER_T);
  const light = cellLight(l, frame);
  const text = webRows(m.cols, m.rows);
  const rows = Array.from({ length: m.rows }, (_, r) => {
    const runs: Run[] = [];
    for (let c = 0; c < m.cols; c++) {
      const i = (r * m.cols + c) * 4;
      const k = cellLook(light[i], light[i + 1], light[i + 2]);
      if (k.alpha < WEB_LOOK.base + 0.012 && k.hue < 0.5) continue;
      const fill = k.hue >= 0.5 ? (k.share > 0.5 ? "var(--lane-orange)" : "var(--lane-teal)") : k.n > 0.35 ? "var(--lane-ink)" : "var(--lane-muted)";
      const opacity = Math.round(k.alpha * 40) / 40;
      const g = rampAt(text[r][c], k.a);
      const last = runs.at(-1);
      if (last && last.c + last.text.length === c && last.fill === fill && last.opacity === opacity) last.text += g;
      else runs.push({ c, text: g, fill, opacity });
    }
    return runs;
  });
  const glow = `web-glow-${l.band}`;
  return (
    <svg viewBox={box(l)} className={className} aria-hidden="true" preserveAspectRatio="xMidYMid slice">
      <defs>
        <radialGradient id={glow}>
          <stop offset="0.4" stopColor="var(--lane-ink)" stopOpacity={frame.glow * 0.3} />
          <stop offset="1" stopColor="var(--lane-ink)" stopOpacity={0} />
        </radialGradient>
      </defs>
      <ellipse cx={l.hub.cx} cy={l.hub.cy} rx={l.plate.w * 1.05} ry={l.plate.h * 1.05} fill={`url(#${glow})`} />
      <g className="dg-mono" fontSize={m.font} letterSpacing={m.spacing} xmlSpace="preserve">
        {rows.map((runs, r) =>
          runs.length ? (
            <text key={r} y={m.baseline(r)}>
              {runs.map((run) => (
                <tspan key={run.c} x={run.c * m.cw + m.spacing / 2} fill={run.fill} fillOpacity={run.opacity}>
                  {run.text}
                </tspan>
              ))}
            </text>
          ) : null,
        )}
      </g>
    </svg>
  );
}

export function webLabel() {
  return (
    "Agent Communication Server at work: a quiet field of the hub's own traffic log fills the background. " +
    "Lines of brighter text join agents such as me, claude-web-1, claude-1, codex-1 and hermes-1, always through hub-server at the centre. " +
    "Each task is a teal pulse sent out to a worker, then small orange progress pulses and one bigger orange result received back; then the line fades and another lights up."
  );
}

export function WebOverlay({ layout: l, className }: { layout: WebLayout; className?: string }) {
  const { plate, hub } = l;
  const compact = l.variant === "tall";
  const size = compact ? 12 : 14;
  const halo = { stroke: "var(--paper)", strokeWidth: compact ? 5 : 6, strokeLinejoin: "round", paintOrder: "stroke" } as const;
  return (
    <svg viewBox={box(l)} className={className} role="img" aria-label={webLabel()} preserveAspectRatio="xMidYMid slice">
      <rect x={plate.x} y={plate.y} width={plate.w} height={plate.h} rx={plate.r} fill="var(--lane-raised)" fillOpacity={0.45} stroke="var(--lane-line)" strokeWidth={1.5} />
      <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" />
      {l.agents.map((a) => (
        <circle key={a.id} cx={a.x} cy={a.y} r={compact ? 3.5 : 4.5} fill="var(--paper)" stroke={a.role === "orchestrator" ? "var(--lane-ink)" : "var(--lane-muted)"} strokeWidth={1.5} />
      ))}
      <g className="dg-mono" fontSize={size} {...halo}>
        {l.agents
          .filter((a) => a.label)
          .map((a) => (
            <text key={a.id} data-agent-label="" x={a.label === "left" ? a.x - 12 : a.x + 12} y={a.y + size * 0.34} textAnchor={a.label === "left" ? "end" : "start"} fill="var(--lane-ink)">
              {a.id}
            </text>
          ))}
        <text x={hub.cx} y={plate.y + plate.h + size * 1.9} textAnchor="middle" fill="var(--lane-ink)">
          hub-server
        </text>
        <text x={hub.cx} y={plate.y + plate.h + size * 3.4} textAnchor="middle" fontSize={size * 0.9}>
          <tspan fill="var(--lane-teal)">sent →</tspan>
          <tspan fill="var(--lane-muted)">{"  ·  "}</tspan>
          <tspan fill="var(--lane-orange)">← received</tspan>
        </text>
      </g>
    </svg>
  );
}
