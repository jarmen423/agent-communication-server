// The ASCII variant's stage, both boxes (1200:440 from md up, 360:440 below):
// the SVG text still as the poster, the glass-hub overlay with haloed labels
// on top, and the live field from ascii.ts once WebGPU is there.
import { TALL, WIDE } from "@/components/hero/geometry";
import { AsciiPoster } from "./ascii-poster";
import { TUNE } from "./field";
import { tallLayout, wideLayout } from "./layout";
import { ExposureLive } from "./live";
import { ExposureOverlay } from "./overlay";
import css from "./hero-b.module.css";

const VARIANTS = [
  { variant: "wide", scene: WIDE, layout: wideLayout(WIDE, TUNE.wide), className: "hero-poster hero-poster-wide" },
  { variant: "tall", scene: TALL, layout: tallLayout(TALL, TUNE.tall), className: "hero-poster hero-poster-tall" },
] as const;

export function AsciiStage() {
  return (
    <>
      {VARIANTS.map((v) => (
        <div key={v.variant} className={v.className} style={{ aspectRatio: `${v.scene.width} / ${v.scene.height}` }}>
          <ExposureLive
            variant={v.variant}
            kind="ascii"
            poster={<AsciiPoster layout={v.layout} className={css.poster} />}
            overlay={<ExposureOverlay layout={v.layout} kind="ascii" className={css.overlay} />}
          />
        </div>
      ))}
    </>
  );
}
