import { lensPath, pointAt, roundedPath, tilePath, tracks, type Pt, type Scene } from "./geometry";

// The poster frame: one moment of a delegation, drawn as a signal-box route
// diagram. me sent task 7f3a to claude-1 (teal route set), claude-1 is
// streaming progress back (orange dots) and its one result is on the way
// (orange tile). codex-1 has a second task arriving; hermes-1 is idle.
const LIT_SENT = new Set(["a", "b1", "b2"]);
const LIT_RECEIVED = new Set(["b1", "a"]);
const PROGRESS: { route: string; t: number }[] = [
  { route: "a", t: 0.14 },
  { route: "a", t: 0.3 },
  { route: "b1", t: 0.2 },
];
const RESULT = { route: "a", t: 0.58, label: "done · 1 result" };
const SECOND_TASK = { route: "b2", t: 0.56, label: "task.c91e" };

export const DIAGRAM_LABEL =
  "Route diagram. The orchestrator me, Claude Code on a laptop, sent a task through hub-server to claude-1 on a workstation. claude-1 is streaming progress back and its one result is on the way to me. A second task is arriving at codex-1 on build-box. hermes-1 on home-server is idle.";

export function RouteDiagram({ scene, className }: { scene: Scene; className?: string }) {
  const { hub, compact } = scene;
  const route = (id: string) => scene.routes.find((r) => r.id === id)!;
  const tile = compact ? { w: 22, h: 16 } : { w: 30, h: 22 };

  return (
    <svg
      viewBox={`0 0 ${scene.width} ${scene.height}`}
      className={className}
      role="img"
      aria-label={DIAGRAM_LABEL}
      preserveAspectRatio="xMidYMid meet"
    >
      {/* Idle track first, then the lit routes over it. */}
      <g fill="none" strokeLinecap="round" strokeLinejoin="round">
        {scene.routes.map((r) => {
          const t = tracks(scene, r);
          return (
            <g key={r.id} stroke="var(--lane-line)" strokeWidth={compact ? 1.5 : 2}>
              <path d={roundedPath(t.sent, scene.bend)} />
              <path d={roundedPath(t.received, scene.bend)} />
            </g>
          );
        })}
        {scene.routes.map((r) => {
          const t = tracks(scene, r);
          return (
            <g key={`lit-${r.id}`}>
              {LIT_SENT.has(r.id) ? <LitTrack d={roundedPath(t.sent, scene.bend)} colour="var(--lane-teal)" compact={compact} /> : null}
              {LIT_RECEIVED.has(r.id) ? <LitTrack d={roundedPath(t.received, scene.bend)} colour="var(--lane-orange)" compact={compact} /> : null}
            </g>
          );
        })}
      </g>

      {/* The server: the lens of the mark on a junction plate. */}
      <rect
        x={hub.cx - hub.size * 0.78}
        y={hub.cy - hub.size * 0.78}
        width={hub.size * 1.56}
        height={hub.size * 1.56}
        rx={compact ? 14 : 20}
        fill="var(--lane-raised)"
        stroke="var(--lane-line)"
      />
      <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" />
      <text
        x={compact ? hub.cx + hub.size * 0.95 : hub.cx}
        y={compact ? hub.cy - 4 : hub.cy + hub.size * 0.78 + 24}
        textAnchor={compact ? "start" : "middle"}
        className="dg-mono"
        fontSize={compact ? 12 : 15}
        fill="var(--lane-ink)"
      >
        hub-server
      </text>
      <text
        x={compact ? hub.cx + hub.size * 0.95 : hub.cx}
        y={compact ? hub.cy + 13 : hub.cy + hub.size * 0.78 + 42}
        textAnchor={compact ? "start" : "middle"}
        className="dg-sans"
        fontSize={compact ? 11 : 13}
        fill="var(--lane-muted)"
      >
        routes by meta.to
      </text>

      {/* Progress: status and event envelopes on the task channel. */}
      {PROGRESS.map((p, i) => {
        const [x, y] = pointAt(tracks(scene, route(p.route)).received, p.t);
        return <circle key={i} cx={x} cy={y} r={compact ? 2.6 : 3.4} fill="var(--lane-orange)" />;
      })}

      <TileOnTrack
        at={pointAt(tracks(scene, route(RESULT.route)).received, RESULT.t)}
        dir="received"
        size={tile}
        label={RESULT.label}
        labelSide={compact ? "left" : "below"}
      />
      <TileOnTrack
        at={pointAt(tracks(scene, route(SECOND_TASK.route)).sent, SECOND_TASK.t)}
        dir="sent"
        size={tile}
        label={SECOND_TASK.label}
        labelSide={compact ? "right" : "above"}
      />

      <Legend scene={scene} />

      {scene.nodes.map((n) => (
        <g key={n.id}>
          <rect x={n.x} y={n.y} width={n.w} height={n.h} rx={compact ? 8 : 10} fill="var(--lane-raised)" stroke="var(--lane-line)" />
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 22 : 32)} className="dg-mono" fontSize={compact ? 13 : 17} fontWeight={500} fill="var(--lane-ink)">
            {n.id}
          </text>
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 40 : 56)} className="dg-sans" fontSize={compact ? 11 : 14} fill="var(--lane-muted)">
            {compact ? n.product : `${n.product} · ${n.machine}`}
          </text>
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 58 : 78)} className="dg-mono" fontSize={compact ? 10.5 : 13} fill="var(--lane-muted)">
            {compact ? n.machine : n.state}
          </text>
        </g>
      ))}

      {compact ? null : (
        <g className="dg-mono" fontSize={13} fill="var(--lane-muted)">
          <text x={32} y={156}>orchestrator</text>
          <text x={944} y={26}>workers</text>
        </g>
      )}
    </svg>
  );
}

function LitTrack({ d, colour, compact }: { d: string; colour: string; compact: boolean }) {
  return (
    <>
      <path d={d} stroke={colour} strokeOpacity={0.16} strokeWidth={compact ? 6 : 9} />
      <path d={d} stroke={colour} strokeWidth={compact ? 1.75 : 2.25} />
    </>
  );
}

function TileOnTrack({
  at,
  dir,
  size,
  label,
  labelSide,
}: {
  at: Pt;
  dir: "sent" | "received";
  size: { w: number; h: number };
  label: string;
  labelSide: "above" | "below" | "left" | "right";
}) {
  const [x, y] = at;
  const colour = dir === "sent" ? "var(--lane-teal)" : "var(--lane-orange)";
  const pos = {
    above: { x, y: y - size.h / 2 - 9, anchor: "middle" as const },
    below: { x, y: y + size.h / 2 + 19, anchor: "middle" as const },
    left: { x: x - size.w / 2 - 8, y: y + 4, anchor: "end" as const },
    right: { x: x + size.w / 2 + 8, y: y + 4, anchor: "start" as const },
  }[labelSide];
  return (
    <g>
      <path d={tilePath(x, y, size.w, size.h, dir)} fill={colour} />
      <text x={pos.x} y={pos.y} textAnchor={pos.anchor} className="dg-mono" fontSize={size.w > 24 ? 13 : 10.5} fill={colour}>
        {label}
      </text>
    </g>
  );
}

/** Direction is labelled in words, not only by colour. */
function Legend({ scene }: { scene: Scene }) {
  const a = scene.routes.find((r) => r.id === "a")!;
  const t = tracks(scene, a);
  if (scene.compact) {
    const [sx, sy] = pointAt(t.sent, 0.3);
    const [rx, ry] = pointAt(t.received, 0.12);
    return (
      <g className="dg-mono" fontSize={10.5}>
        <text x={sx + 9} y={sy + 4} fill="var(--lane-teal)">sent ↓</text>
        <text x={rx - 9} y={ry + 4} textAnchor="end" fill="var(--lane-orange)">↑ received</text>
      </g>
    );
  }
  const [sx, sy] = pointAt(t.sent, 0.05);
  const [rx, ry] = pointAt(t.received, 0.05);
  return (
    <g className="dg-mono" fontSize={13}>
      <text x={sx} y={sy - 13} fill="var(--lane-teal)">sent →</text>
      <text x={rx} y={ry + 24} fill="var(--lane-orange)">← received</text>
    </g>
  );
}
