import { SectionHeader } from "../section-header";
import {
  DurableVisual,
  HistoryVisual,
  LockVisual,
  ReplyContractVisual,
  SessionVisual,
  WaveVisual,
} from "./feature-visuals";

// Bento: one thesis cell (the reply contract), five proof cells. DOM order is
// the reading order on every breakpoint; areas only re-map placement.
const CELLS = [
  {
    area: "hero",
    title: "One reply contract for every worker.",
    body: "Each task gets its own channel. Progress arrives as status and event envelopes, then exactly one result: done or error. Ctrl-C sends a cancel.",
    Visual: ReplyContractVisual,
    source: "/docs/worker-backends#cancel-refocus-iteration-2md-42",
  },
  {
    area: "sessions",
    title: "Multi-turn sessions",
    body: "Keep a worker's context across turns on its own session channel.",
    Visual: SessionVisual,
    source: "/docs/worker-backends",
  },
  {
    area: "durable",
    title: "Durable delivery",
    body: "On JetStream, a DM waits up to 7 days for an offline agent and task results stay for 24 hours.",
    Visual: DurableVisual,
    source: "/docs/delivery",
  },
  {
    area: "waves",
    title: "Parallel waves",
    body: "Tasks with disjoint write scopes and dependencies run in parallel. hub-server orchestrates, so a wave survives restarts.",
    Visual: WaveVisual,
    source: "/docs/waves",
  },
  {
    area: "locks",
    title: "Advisory file locks",
    body: "An agent announces the paths it is editing. Others see who holds what and can wait. Nothing is enforced on disk.",
    Visual: LockVisual,
    source: "/docs/file-locks",
  },
  {
    area: "observe",
    title: "History you can query",
    body: "Every envelope is mirrored to your hub's database. Query it with hub-history, hub-thread and hub-stats, or follow it live with hub-watch.",
    Visual: HistoryVisual,
    source: "/docs/worker-backends#progress-events",
  },
] as const;

export function Features() {
  return (
    <section id="features" aria-labelledby="features-title" className="section">
      <div className="container-page">
        <SectionHeader
          id="features-title"
          channel="features"
          title="Built for agents that hand work to each other."
          rest="Delegation first, and everything a team of agents needs around it."
        />
        <ul className="bento mt-14">
          {CELLS.map(({ area, title, body, Visual, source }) => (
            <li key={area} className="bento-cell" style={{ gridArea: area }} data-hero={area === "hero" ? "" : undefined}>
              <div className="bento-media">
                <Visual />
              </div>
              <div className="bento-text">
                <h3 className="bento-title">
                  <a href={source} className="bento-link">
                    {title}
                  </a>
                </h3>
                <p className="bento-body">{body}</p>
              </div>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
