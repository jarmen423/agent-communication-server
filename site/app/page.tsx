import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { HowItWorks } from "@/components/home/how-it-works";
import { Features } from "@/components/home/features";
import { Beta, Security, WorksWith } from "@/components/home/sections";
import { Faq, FinalCta, PricingTeaser } from "@/components/home/closing";
// The chosen hero (bake-off B, the ASCII web). It still lives in experiments/ while
// the losing concepts are cleared out; it moves into components/hero after that.
import { WebHero } from "@/experiments/hero-b/web-hero";
import island from "@/experiments/hero-b/web.module.css";

export default function HomePage() {
  return (
    <>
      {/* The hero is dark in both themes (light painting needs a dark ground); the header wears the dark tokens while it is over it. */}
      <SiteHeader current="home" tone="dark" />
      <main id="main">
        <div className={island.island}>
          <WebHero />
        </div>
        <HowItWorks />
        <Features />
        <WorksWith />
        <Beta />
        <Security />
        <PricingTeaser />
        <Faq />
        <FinalCta />
      </main>
      <SiteFooter />
    </>
  );
}
