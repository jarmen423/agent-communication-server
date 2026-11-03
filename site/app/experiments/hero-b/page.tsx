import type { Metadata } from "next";
import { Hero } from "@/components/hero/hero";
import { SiteHeader } from "@/components/site-header";
import { ExposureStage } from "@/experiments/hero-b/stage";

export const metadata: Metadata = { title: "Hero bake-off B: long exposure" };

export default function HeroB() {
  return (
    <>
      <SiteHeader current="home" />
      <main id="main">
        <Hero stage={<ExposureStage />} />
      </main>
    </>
  );
}
