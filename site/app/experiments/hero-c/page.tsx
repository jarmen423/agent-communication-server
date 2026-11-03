import type { Metadata } from "next";
import { Hero } from "@/components/hero/hero";
import { TALL, WIDE } from "@/components/hero/geometry";
import { SiteHeader } from "@/components/site-header";
import { TapeOverlay } from "@/experiments/hero-c/overlay";
import { TapePoster } from "@/experiments/hero-c/poster";
import { TALL_TAPE, WIDE_TAPE } from "@/experiments/hero-c/schedule";
import { StripStage } from "@/experiments/hero-c/stage";

export const metadata: Metadata = { title: "Hero C · strip chart" };

const LABEL =
  "Strip chart of the hub, like a live hub-watch. One trace per agent, with the hub-server rail between orchestrators and workers. " +
  "At 12:04:07 me, on a laptop, sent task 7f3a2c1e to claude-1 on a workstation; claude-1 streamed status and progress events and sent one result back at 12:04:20. " +
  "At 12:04:09 me spawned the wave docs-pass, and hub-server dispatched it to codex-1 on build-box and hermes-1 on home-server; both are still streaming progress. " +
  "Earlier, claude-web-1 on claude.ai delegated a task to hermes-1 and got one result back.";

const LABEL_TALL = LABEL.replace(" Earlier, claude-web-1 on claude.ai delegated a task to hermes-1 and got one result back.", "");

export default function HeroC() {
  return (
    <>
      <SiteHeader current="home" />
      <main id="main">
        <Hero
          stage={
            <>
              <div className="hero-poster hero-poster-wide" style={{ aspectRatio: `${WIDE.width} / ${WIDE.height}` }}>
                <StripStage variant="wide" label={LABEL} poster={<TapePoster L={WIDE_TAPE} id="hc-wide" />} overlay={<TapeOverlay L={WIDE_TAPE} id="hc-wide-o" />} />
              </div>
              <div className="hero-poster hero-poster-tall" style={{ aspectRatio: `${TALL.width} / ${TALL.height}` }}>
                <StripStage variant="tall" label={LABEL_TALL} poster={<TapePoster L={TALL_TAPE} id="hc-tall" />} overlay={<TapeOverlay L={TALL_TAPE} id="hc-tall-o" />} />
              </div>
            </>
          }
        />
      </main>
    </>
  );
}
