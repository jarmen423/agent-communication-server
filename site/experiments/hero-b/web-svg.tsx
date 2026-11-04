// Server-rendered SVG for the ASCII web, in the same design space as the live
// canvas and cover-mapped the same way (preserveAspectRatio "slice"):
//  - WebPoster: the field and the cells lit at the poster moment, by
//    reference (<use>) into a cached static document (web-poster-svg.ts), so
//    the HTML only carries a few elements. Only the shown band is rendered.
//  - WebOverlay: the hub (glass plate and the lens of the mark), agent ports,
//    a few haloed names and direction in words, per band.
import { lensPath } from "@/components/hero/geometry";
import { WEB, type Band, type WebLayout } from "./web-model";
import poster from "./posters/web.svg";

const box = (l: WebLayout) => `0 0 ${l.width} ${l.height}`;
const DOC: { src: string } = poster;

/** One layer of the poster document (`field` or `lit-<band>`), inheriting the page's mono face. */
export function WebPoster({ variant, layer, className }: { variant: "wide" | "tall"; layer: "field" | Band; className?: string }) {
  const l = variant === "wide" ? WEB.M : WEB.tall;
  return (
    <svg viewBox={box(l)} className={`dg-mono ${className ?? ""}`} aria-hidden="true" preserveAspectRatio="xMidYMid slice">
      <use href={`${DOC.src}#${layer === "field" ? `field-${variant}` : `lit-${layer}`}`} />
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
