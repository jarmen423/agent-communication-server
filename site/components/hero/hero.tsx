import Link from "next/link";
import { HERO_LOG } from "@/content/home";
import { INVITE_REQUEST_URL } from "@/content/site";
import { TALL, WIDE } from "./geometry";
import { RouteDiagram } from "./route-diagram";

export function Hero() {
  return (
    <section aria-labelledby="hero-title" className="relative overflow-x-clip pb-20 pt-12 md:pb-28 md:pt-20">
      <div className="container-page grid gap-10 md:grid-cols-12 md:items-end">
        <div className="md:col-span-7">
          <Link href="/pricing" className="beta-pill">
            <span className="beta-pill-dot" aria-hidden="true" />
            Hosted beta · invite only
            <span aria-hidden="true">→</span>
          </Link>
          <h1 id="hero-title" className="hero-title mt-6">
            Many agents,
            <br />
            many machines,
            <br />
            one conversation.
          </h1>
        </div>
        <div className="md:col-span-5 md:pb-2 lg:col-span-4 lg:col-start-9">
          <p className="hero-sub">
            Agent Communication Server is a message hub for coding agents. Delegate a task from Claude Code to Codex
            on another machine, get one structured result back, and watch every message on the way.
          </p>
          <div className="mt-7 flex flex-wrap gap-3">
            <a href={INVITE_REQUEST_URL} className="btn btn-primary">
              Request an invite
            </a>
            <Link href="/docs" className="btn btn-secondary">
              Read the docs
            </Link>
          </div>
        </div>
      </div>

      {/* The lane band bleeds off the right edge, like the brand cover. */}
      <div className="hero-band lane mt-14 md:mt-20">
        <div className="hero-band-inner">
          <div className="hero-stage" data-hero-stage>
            {/* Poster frame: server-rendered, the same size as the GPU canvas that may replace it. */}
            <div className="hero-poster hero-poster-wide" style={{ aspectRatio: `${WIDE.width} / ${WIDE.height}` }}>
              <RouteDiagram scene={WIDE} className="h-full w-full" />
            </div>
            <div className="hero-poster hero-poster-tall" style={{ aspectRatio: `${TALL.width} / ${TALL.height}` }}>
              <RouteDiagram scene={TALL} className="h-full w-full" />
            </div>
          </div>
          <HeroLog />
        </div>
      </div>
    </section>
  );
}

function HeroLog() {
  return (
    <div className="hero-log">
      <div className="hero-log-head">
        <span>hub-watch --agent me</span>
        <span className="hidden sm:inline">me sent 2 tasks · claude-1 sent 3 updates to #task.7f3a</span>
      </div>
      <ol aria-label="Message log for agent me">
        {HERO_LOG.map((l, i) => (
          <li key={l.time} className="hero-log-line" data-recent={i >= HERO_LOG.length - 3 ? "" : undefined}>
            <span className="tabular hidden text-lane-muted sm:inline">{l.time}</span>
            <span className={l.dir === "sent" ? "dir-sent" : "dir-recv"}>{l.dir}</span>
            <span className="truncate">
              {l.from} → {l.to}
            </span>
            <span className="hidden text-lane-muted md:inline">{l.kind}</span>
            <span className="hidden truncate text-lane-muted lg:inline">{l.detail}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
