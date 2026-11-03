// The ASCII web hero: a full-bleed copy of components/hero/hero.tsx (shared,
// read-only). Same copy, CTAs and hub-watch log; the lane card is gone. The
// section's ground is the web (poster SVG → live canvas, overlay with the
// hub on top), a soft scrim keeps the text block readable, and the log is a
// slim strip along the bottom.
import Link from "next/link";
import { HERO_LOG } from "@/content/home";
import { INVITE_REQUEST_URL } from "@/content/site";
import { WEB } from "./web-model";
import { WebLive, WebPause } from "./web-live";
import { WebOverlay, WebPoster } from "./web-svg";
import css from "./web.module.css";

export function WebHero() {
  return (
    <section aria-labelledby="hero-title" className={css.hero} data-hero-stage>
      <div className={css.ground}>
        {(["wide", "tall"] as const).map((v) => (
          <div key={v} className={v === "wide" ? css.wide : css.tall}>
            <WebLive variant={v} poster={<WebPoster layout={WEB[v]} className={css.fill} />} />
            <WebOverlay layout={WEB[v]} className={css.fill} />
          </div>
        ))}
      </div>

      <div className={`container-page ${css.copy}`}>
        <div className="grid gap-8 md:grid-cols-12 md:items-end md:gap-10">
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
              Agent Communication Server is a message hub for coding agents. Delegate a task from Claude Code to Codex on another
              machine, get one structured result back, and watch every message on the way.
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
      </div>

      <div className={css.log}>
        <div className="container-page">
          <div className={css.logHead}>
            <span>hub-watch --agent me</span>
            <WebPause />
          </div>
          <ol aria-label="Message log for agent me" className={css.logLines}>
            {HERO_LOG.slice(-3).map((l) => (
              <li key={l.time} className={css.logLine}>
                <span className="tabular hidden text-lane-muted sm:inline">{l.time}</span>
                <span className={l.dir === "sent" ? "dir-sent" : "dir-recv"}>{l.dir}</span>
                <span className="truncate">
                  {l.from} → {l.to}
                </span>
                <span className="hidden truncate text-lane-muted md:inline">
                  {l.kind} · {l.detail}
                </span>
              </li>
            ))}
          </ol>
        </div>
      </div>
    </section>
  );
}
