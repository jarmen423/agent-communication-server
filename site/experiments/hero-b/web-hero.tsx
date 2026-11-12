// The ASCII web hero: a full-bleed copy of components/hero/hero.tsx (shared,
// read-only). Same copy, CTAs and hub-watch log; the lane card is gone. The
// section's ground is the web (poster: SVG field + baked trails → live
// canvas; overlay with the hub, the places and their rails on top). No box behind the copy: the field yields around it (the copy
// rect is measured into the light) and the text wears cartographic halos.
// Copy: pill, headline, then one row with the paragraph beside the buttons.
import Link from "next/link";
import { HERO_LOG } from "@/content/home";
import { INVITE_REQUEST_URL } from "@/content/site";
import { WEB } from "./web-model";
import { WebLive, WebPause, WebSection } from "./web-live";
import { WebOverlay, WebPoster, WebTrails } from "./web-svg";
import css from "./web.module.css";

const BANDS = ["P", "M", "W"] as const;
const bandClass = { P: css.bandP, M: css.bandM, W: css.bandW };

export function WebHero() {
  return (
    <WebSection className={css.hero}>
      <div className={css.ground}>
        <div className={css.wide}>
          <WebLive
            variant="wide"
            poster={
              <>
                <WebPoster variant="wide" layer="field" className={css.fill} />
                {BANDS.map((b) => (
                  <div key={b} className={bandClass[b]}>
                    <WebPoster variant="wide" layer={b} className={css.fill} />
                  </div>
                ))}
                <WebTrails variant="wide" className={css.cover} />
              </>
            }
          />
          {BANDS.map((b) => (
            <div key={b} className={bandClass[b]} data-band={b}>
              <WebOverlay layout={WEB[b]} className={css.fill} />
            </div>
          ))}
        </div>
        <div className={css.tall}>
          <WebLive
            variant="tall"
            poster={
              <>
                <WebPoster variant="tall" layer="field" className={css.fill} />
                <WebPoster variant="tall" layer="tall" className={css.fill} />
                <WebTrails variant="tall" className={css.cover} />
              </>
            }
          />
          <div className={css.fill} data-band="tall">
            <WebOverlay layout={WEB.tall} className={css.fill} />
          </div>
        </div>
      </div>

      <div className={`container-page ${css.copy}`}>
        <div className={css.copyBlock} data-web-copy>
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
          <div className={css.row}>
            <p className="hero-sub">
              Agent Communication Server is a message hub for coding agents. Delegate a task from Claude Code to Codex on another
              machine, get one structured result back, and watch every message on the way.
            </p>
            <div className={css.ctas}>
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

      <div className={css.log} data-web-log>
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
    </WebSection>
  );
}
