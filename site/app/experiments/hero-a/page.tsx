import type { Metadata } from "next";
import { TALL, WIDE } from "@/components/hero/geometry";
import { Hero } from "@/components/hero/hero";
import { SiteHeader } from "@/components/site-header";
import { SignalBoxPoster } from "@/experiments/hero-a/poster";
import { SignalBoxStage } from "@/experiments/hero-a/stage";

export const metadata: Metadata = { title: "Hero A · signal box" };

// Concept A: the route diagram as a live signal-box panel. Posters are server-rendered;
// the stage is the only client component, and the GPU module loads after first paint.
export default function HeroA() {
  return (
    <>
      <SiteHeader current="home" />
      <main id="main">
        <Hero stage={<SignalBoxStage wide={<SignalBoxPoster scene={WIDE} />} tall={<SignalBoxPoster scene={TALL} />} />} />
      </main>
    </>
  );
}
