// Server-rendered stills for the ASCII web, in the same design space as the
// live canvas and cover-mapped the same way (SVG "slice", CSS object-fit cover):
//  - WebPoster: the field and the hub's glow, by reference (<use>) into a
//    cached static document (web-poster-svg.ts), so the HTML only carries a
//    few elements.
//  - WebTrails: the tracks' long exposure at the poster moment, a baked PNG
//    per band; only the band on screen is fetched.
//  - WebOverlay: the hub (glass plate and the lens of the mark), agent ports,
//    a few haloed names and direction in words, per band.
import { lensPath } from "@/components/hero/geometry";
import { WEB, WEB_POSTER_T, type Band, type WebLayout } from "./web-model";
import { placeAlpha } from "./web-places";
import poster from "./posters/web.svg";
import trailsM from "./posters/web-trails-M.png";
import trailsP from "./posters/web-trails-P.png";
import trailsTall from "./posters/web-trails-tall.png";
import trailsW from "./posters/web-trails-W.png";
import { BAND_QUERY } from "./web-bands";

const box = (l: WebLayout) => `0 0 ${l.width} ${l.height}`;
const DOC: { src: string } = poster;

/** One layer of the poster document (`field` or `hub-<band>`), inheriting the page's mono face. */
export function WebPoster({ variant, layer, className }: { variant: "wide" | "tall"; layer: "field" | Band; className?: string }) {
  const l = variant === "wide" ? WEB.M : WEB.tall;
  return (
    <svg viewBox={box(l)} className={`dg-mono ${className ?? ""}`} aria-hidden="true" preserveAspectRatio="xMidYMid slice">
      <use href={`${DOC.src}#${layer === "field" ? `field-${variant}` : `hub-${layer}`}`} />
    </svg>
  );
}

/** 1×1 transparent GIF: where a ground is hidden (CSS, md = 768px) its still is never fetched. */
const NOTHING = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

/**
 * The tracks at the poster moment. Indexed PNGs (11–40 KB), served as baked:
 * a plain <picture> on purpose (no JS; next/image would hydrate a client
 * component for nothing). Media queries match the band rules in web.module.css.
 */
export function WebTrails({ variant, className }: { variant: "wide" | "tall"; className?: string }) {
  if (variant === "tall")
    return (
      <picture>
        <source media="(min-width: 768px)" srcSet={NOTHING} />
        <img src={trailsTall.src} width={trailsTall.width} height={trailsTall.height} alt="" className={className} />
      </picture>
    );
  return (
    <picture>
      <source media="(max-width: 767px)" srcSet={NOTHING} />
      <source media={BAND_QUERY.W} srcSet={trailsW.src} width={trailsW.width} height={trailsW.height} />
      <source media={BAND_QUERY.M} srcSet={trailsM.src} width={trailsM.width} height={trailsM.height} />
      <img src={trailsP.src} width={trailsP.width} height={trailsP.height} alt="" className={className} />
    </picture>
  );
}

export function webLabel() {
  return (
    "Agent Communication Server at work: a quiet field of the hub's own traffic log fills the background. " +
    "Around hub-server sit places such as laptop-1, website-2, teammate-2, cloud-1, office-3 and home-1, each with different agents (Claude Code, Codex, Hermes, Cursor, Grok, OpenCode, Kilo, Antigravity, chat apps), and places come and go. " +
    "Every place is wired to hub-server by a track with two lanes, and every message travels through the hub as light that fades slowly, like a long exposure. " +
    "Each task is a teal trail sent out from an orchestrator to a worker in another place; status, progress and one result come back as orange trails received on the other lane."
  );
}

export function WebOverlay({ layout: l, className }: { layout: WebLayout; className?: string }) {
  const { plate, hub } = l;
  const compact = l.variant === "tall";
  const alphaAt = (i: number, j: number) => Math.round(placeAlpha(i, l.slots[i], j, WEB_POSTER_T) * 100) / 100;
  const size = compact ? 12 : 14;
  const halo = { stroke: "var(--paper)", strokeWidth: compact ? 5 : 6, strokeLinejoin: "round", paintOrder: "stroke" } as const;
  return (
    <svg viewBox={box(l)} className={className} role="img" aria-label={webLabel()} preserveAspectRatio="xMidYMid slice">
      {/* Rails, cards: in the cached poster document; the live layer fades them via opacity. */}
      {l.slots.map((slot, i) =>
        slot.places.map((pid, j) => <use key={pid} data-rail={`${i}:${j}`} href={`${DOC.src}#rail-${l.band}-${i}-${j}`} opacity={alphaAt(i, j)} />),
      )}
      <rect x={plate.x} y={plate.y} width={plate.w} height={plate.h} rx={plate.r} fill="var(--lane-raised)" fillOpacity={0.45} stroke="var(--lane-line)" strokeWidth={1.5} />
      <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" />
      <g className="dg-mono">
        {l.slots.map((slot, i) =>
          slot.places.map((pid, j) => <use key={pid} data-place={`${i}:${j}`} href={`${DOC.src}#card-${l.band}-${i}-${j}`} opacity={alphaAt(i, j)} />),
        )}
      </g>
      <g className="dg-mono" fontSize={size} {...halo}>
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
