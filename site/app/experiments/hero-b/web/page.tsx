import type { Metadata } from "next";
import { SiteHeader } from "@/components/site-header";
import { geistSerif } from "@/experiments/hero-b/web-font";
import { WebHero } from "@/experiments/hero-b/web-hero";
import css from "@/experiments/hero-b/web.module.css";

export const metadata: Metadata = { title: "Hero bake-off B: the ASCII web" };

// Header and hero share one dark island, so the header reads over the field in both themes.
export default function HeroBWeb() {
  return (
    <div className={`${css.island} ${geistSerif.variable}`}>
      <SiteHeader current="home" />
      <main id="main">
        <WebHero />
      </main>
    </div>
  );
}
