import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { Hero } from "@/components/hero/hero";
import { HowItWorks } from "@/components/home/how-it-works";
import { Features } from "@/components/home/features";
import { Beta, Security, WorksWith } from "@/components/home/sections";
import { Faq, FinalCta, PricingTeaser } from "@/components/home/closing";

export default function HomePage() {
  return (
    <>
      <SiteHeader current="home" />
      <main id="main">
        <Hero />
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
