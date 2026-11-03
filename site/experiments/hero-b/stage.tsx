// Concept B's hero stage, both boxes: 1200:440 from md up, 360:440 below.
// The poster is a still of the same exposure the live layer starts from
// (baked from the real shaders by tools/bake.mjs), under the shared overlay.
import type { StaticImageData } from "next/image";
import { TALL, WIDE } from "@/components/hero/geometry";
import { tallLayout, wideLayout, type Layout } from "./layout";
import { createTraffic, hubLamp, POSTER_T } from "./schedule";
import { ExposureLive } from "./live";
import { ExposureOverlay } from "./overlay";
import css from "./hero-b.module.css";
import tallPoster from "./posters/tall.png";
import widePoster from "./posters/wide.png";

const VARIANTS = [
  { variant: "wide", scene: WIDE, layout: wideLayout(WIDE), poster: widePoster, hidden: "(max-width: 767.98px)", className: "hero-poster hero-poster-wide" },
  { variant: "tall", scene: TALL, layout: tallLayout(TALL), poster: tallPoster, hidden: "(min-width: 768px)", className: "hero-poster hero-poster-tall" },
] as const;

export function ExposureStage() {
  return (
    <>
      {VARIANTS.map((v) => (
        <div key={v.variant} className={v.className} style={{ aspectRatio: `${v.scene.width} / ${v.scene.height}` }}>
          <ExposureLive
            variant={v.variant}
            poster={<Poster src={v.poster} hidden={v.hidden} layout={v.layout} />}
            overlay={<ExposureOverlay layout={v.layout} className={css.overlay} />}
          />
        </div>
      ))}
    </>
  );
}

/** 1×1 transparent GIF: where this variant is hidden (CSS, md = 768px) its still is never fetched. */
const NOTHING = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

function Poster({ src, hidden, layout }: { src: StaticImageData; hidden: string; layout: Layout }) {
  // Indexed PNG, 40–55 KB, served as baked. A plain <img> on purpose: the still
  // needs no JS, and next/image would hydrate a client component for nothing.
  // The hub's neutral glow at the poster moment sits on top in SVG.
  const { glow } = hubLamp(createTraffic(layout, 7, { through: true }).live(POSTER_T), POSTER_T);
  const { plate } = layout;
  const id = `b-glow-${layout.width}`;
  return (
    <>
      <svg viewBox={`0 0 ${layout.width} ${layout.height}`} className={css.overlay} aria-hidden="true">
        <defs>
          <radialGradient id={id}>
            <stop offset="0.55" stopColor="var(--lane-ink)" stopOpacity={glow * 0.26} />
            <stop offset="1" stopColor="var(--lane-ink)" stopOpacity={0} />
          </radialGradient>
        </defs>
        <ellipse cx={plate.x + plate.w / 2} cy={plate.y + plate.h / 2} rx={plate.w * 0.85} ry={plate.h * 0.85} fill={`url(#${id})`} />
      </svg>
      <picture>
        <source media={hidden} srcSet={NOTHING} />
        <img src={src.src} width={src.width} height={src.height} alt="" fetchPriority="high" className={css.poster} />
      </picture>
    </>
  );
}
