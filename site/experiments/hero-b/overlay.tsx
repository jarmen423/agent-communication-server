// The annotation layer, shared by the poster and the live layer so nothing
// moves when the GPU fades in: the hub (the lens of the mark on its plate),
// machines as trays of agent ports, a few names, direction in words, and a
// caption that says what kind of picture this is. Server-rendered SVG.
import { lensPath } from "@/components/hero/geometry";
import type { Layout, Machine } from "./layout";

export function exposureLabel(layout: Layout) {
  const names = (role: "orchestrator" | "worker") =>
    layout.machines.filter((m) => m.role === role).map((m) => `${m.agents.map((a) => a.id).join(", ")} on ${m.id}`);
  return (
    `Long exposure of agent traffic through one hub, hub-server. Each light trail is one message between two agents, and trails fade over about 20 seconds. ` +
    `Orchestrators (${names("orchestrator").join("; ")}) send tasks as teal trails through the hub to ${layout.agents.length - 4} workers ` +
    `(${names("worker").join("; ")}). Each worker streams progress back as short orange ticks, then one brighter orange result.`
  );
}

export function ExposureOverlay({ layout, className }: { layout: Layout; className?: string }) {
  const { plate, hub, compact } = layout;
  const mono = compact ? 10.5 : 12.5;
  return (
    <svg viewBox={`0 0 ${layout.width} ${layout.height}`} className={className} role="img" aria-label={exposureLabel(layout)} preserveAspectRatio="xMidYMid meet">
      <rect x={plate.x} y={plate.y} width={plate.w} height={plate.h} rx={plate.r} fill="var(--lane-raised)" stroke="var(--lane-line)" />
      <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" />

      {layout.machines.map((m) => (
        <Tray key={m.id} m={m} compact={compact} />
      ))}

      <g className="dg-mono" fontSize={mono}>
        {layout.machines.map((m) => (
          <text key={m.id} x={m.label.x} y={m.label.y} textAnchor={m.label.anchor} fill="var(--lane-muted)">
            {m.id}
          </text>
        ))}
        {layout.agents
          .filter((a) => a.label)
          .map((a) => (
            <text key={a.id} x={a.label === "before" ? a.x - 15 : a.x + 15} y={a.y + 4} textAnchor={a.label === "before" ? "end" : "start"} fill="var(--lane-ink)">
              {a.id}
            </text>
          ))}
      </g>

      {compact ? (
        <g className="dg-mono" fontSize={10.5}>
          <text x={hub.cx + plate.w / 2 + 10} y={hub.cy + 4} fill="var(--lane-ink)">hub-server</text>
          <text x={240} y={164} fill="var(--lane-teal)">sent ↓</text>
          <text x={120} y={164} textAnchor="end" fill="var(--lane-orange)">↑ received</text>
          <text x={280} y={56} textAnchor="end" fill="var(--lane-ink)">me</text>
        </g>
      ) : (
        <g className="dg-mono" fontSize={13}>
          <text x={hub.cx} y={plate.y + plate.h + 26} textAnchor="middle" fill="var(--lane-ink)">hub-server</text>
          <text x={436} y={166} fill="var(--lane-teal)">sent →</text>
          <text x={436} y={276} fill="var(--lane-orange)">← received</text>
          <text x={32} y={424} fontSize={12} fill="var(--lane-muted)">
            20 s exposure · {layout.agents.length} agents · {layout.machines.length} machines · one hub
          </text>
        </g>
      )}
    </svg>
  );
}

/** A machine: a rounded tray holding its agents' ports, where their trails plug in. */
function Tray({ m, compact }: { m: Machine; compact: boolean }) {
  const xs = m.agents.map((a) => a.x);
  const ys = m.agents.map((a) => a.y);
  const pad = compact ? 6.5 : 8.5;
  const x = Math.min(...xs) - pad;
  const y = Math.min(...ys) - pad;
  const r = compact ? 2.6 : 3.2;
  return (
    <g>
      <rect x={x} y={y} width={Math.max(...xs) - x + pad} height={Math.max(...ys) - y + pad} rx={pad} fill="var(--lane-raised)" stroke="var(--lane-line)" />
      {m.agents.map((a) => (
        <circle key={a.id} cx={a.x} cy={a.y} r={r} fill="var(--lane)" stroke={a.role === "orchestrator" ? "var(--lane-ink)" : "var(--lane-muted)"} strokeWidth={compact ? 1 : 1.25} />
      ))}
    </g>
  );
}
