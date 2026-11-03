// Concept B's hero stage, both boxes: 1200:440 from md up, 360:440 below.
// The poster is a still of the same exposure the live layer starts from
// (baked from the real shaders by tools/bake.mjs), under the shared overlay.
import type { StaticImageData } from "next/image";
import { TALL, WIDE } from "@/components/hero/geometry";
import { tallLayout, wideLayout } from "./layout";
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
            poster={<Poster src={v.poster} hidden={v.hidden} />}
            overlay={<ExposureOverlay layout={v.layout} className={css.overlay} />}
          />
        </div>
      ))}
    </>
  );
}

/** 1×1 transparent GIF: where this variant is hidden (CSS, md = 768px) its still is never fetched. */
const NOTHING = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

function Poster({ src, hidden }: { src: StaticImageData; hidden: string }) {
  // Indexed PNG, 40–55 KB, served as baked. A plain <img> on purpose: the still
  // needs no JS, and next/image would hydrate a client component for nothing.
  return (
    <picture>
      <source media={hidden} srcSet={NOTHING} />
      <img src={src.src} width={src.width} height={src.height} alt="" fetchPriority="high" className={css.poster} />
    </picture>
  );
}
