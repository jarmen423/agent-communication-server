import type { Metadata } from "next";
import { Hero } from "@/components/hero/hero";
import { SiteHeader } from "@/components/site-header";
import { AsciiStage } from "@/experiments/hero-b/ascii-stage";

export const metadata: Metadata = { title: "Hero bake-off B: long exposure, ASCII field" };

export default function HeroBAscii() {
  return (
    <>
      <SiteHeader current="home" />
      <main id="main">
        <Hero stage={<AsciiStage />} />
      </main>
    </>
  );
}
