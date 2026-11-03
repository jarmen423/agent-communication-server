// The signal box's poster frame: the shared route diagram's drawing, set at
// POSTER_T of the loop. Server-rendered, no client JS. Its moving parts sit in
// .ha-dyn, which fades out when the GPU layer takes over; node states and route
// labels carry data attributes so the live layer can keep them current.
import { lensPath, roundedPath, tilePath, tracks, type Scene } from "@/components/hero/geometry";
import { sizes } from "./board";
import { buildModel, frameAt, POSTER_T, TASKS } from "./script";
import { subPath, type Track } from "./tracks";

export const POSTER_LABEL =
  "Signal box route diagram. me, Claude Code on a laptop, delegates a task through hub-server to claude-1 on a workstation; claude-1 streams progress back and returns one result. Then a wave sends tasks to codex-1 on build-box and hermes-1 on home-server in parallel, and both results return to me. Sent messages run on the teal track, received ones on the orange track.";

/** Route channel labels (wide only): right-aligned above each worker route's last straight. */
const ROUTE_LABEL: Record<string, [number, number]> = { b1: [934, 65], b2: [934, 197], b3: [934, 329] };

/** The lamps the shader would show lit for [lo, hi], as one section-aligned run. */
function litRun(t: Track, lo: number, hi: number, pitch: number): [number, number] | null {
  const ramp = pitch * 0.75;
  let first = -1;
  let last = -1;
  for (let i = 0; i * pitch < t.length; i++) {
    const s0 = i * pitch;
    const on = Math.min(1, Math.max(0, (hi - s0) / ramp)) * Math.min(1, Math.max(0, (s0 + pitch - lo) / ramp));
    if (on >= 0.5) {
      if (first < 0) first = i;
      last = i;
    }
  }
  return first < 0 ? null : [first * pitch, Math.min(t.length, (last + 1) * pitch)];
}

export function SignalBoxPoster({ scene }: { scene: Scene }) {
  const { hub, compact } = scene;
  const model = buildModel(scene);
  const f = frameAt(model, POSTER_T);
  const z = sizes(compact);
  const colour = (dir: string) => (dir === "sent" ? "var(--lane-teal)" : "var(--lane-orange)");

  return (
    <svg viewBox={`0 0 ${scene.width} ${scene.height}`} className="h-full w-full" role="img" aria-label={POSTER_LABEL} preserveAspectRatio="xMidYMid meet">
      <g fill="none" stroke="var(--lane-line)" strokeWidth={compact ? 1.5 : 2} strokeLinecap="round" strokeLinejoin="round">
        {scene.routes.map((r) => {
          const t = tracks(scene, r);
          return (
            <g key={r.id}>
              <path d={roundedPath(t.sent, scene.bend)} />
              <path d={roundedPath(t.received, scene.bend)} />
            </g>
          );
        })}
      </g>

      <g className="ha-dyn" fill="none" strokeLinecap="round" strokeLinejoin="round">
        {model.tracks.map((t) => {
          const [lo, hi] = f.lit[t.k];
          const run = hi > lo ? litRun(t, lo, hi, z.pitch) : null;
          if (!run) return null;
          const d = subPath(t, run[0], run[1]);
          return (
            <g key={t.k} stroke={colour(t.dir)}>
              <path d={d} strokeOpacity={0.16} strokeWidth={z.sigma * 2} />
              <path d={d} strokeWidth={z.lamp} strokeDasharray={`${z.pitch - z.gap - z.lamp} ${z.gap + z.lamp}`} strokeDashoffset={-(z.gap + z.lamp) / 2} />
            </g>
          );
        })}
        {f.sprites.map((s, i) =>
          s.kind === "pulse" ? (
            <circle key={i} cx={s.x} cy={s.y} r={z.pulse + z.lamp * 0.3} fill={colour(s.dir)} stroke="var(--lane)" strokeWidth={z.lamp * 0.6} />
          ) : (
            <path key={i} d={tilePath(s.x, s.y, z.tile[0], z.tile[1], s.dir, z.tile[2])} fill={colour(s.dir)} stroke="none" />
          ),
        )}
      </g>

      {/* The server: the lens of the mark on a junction plate. */}
      <rect x={hub.cx - hub.size * 0.78} y={hub.cy - hub.size * 0.78} width={hub.size * 1.56} height={hub.size * 1.56} rx={z.plateR} fill="var(--lane-raised)" stroke="var(--lane-line)" />
      <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" />
      <text x={compact ? hub.cx + hub.size * 0.95 : hub.cx} y={compact ? hub.cy - 4 : hub.cy + hub.size * 0.78 + 24} textAnchor={compact ? "start" : "middle"} className="dg-mono" fontSize={compact ? 12 : 15} fill="var(--lane-ink)">
        hub-server
      </text>
      <text x={compact ? hub.cx + hub.size * 0.95 : hub.cx} y={compact ? hub.cy + 13 : hub.cy + hub.size * 0.78 + 42} textAnchor={compact ? "start" : "middle"} className="dg-sans" fontSize={compact ? 11 : 13} fill="var(--lane-muted)">
        routes by meta.to
      </text>

      <Legend scene={scene} track={model.tracks[0]} back={model.tracks[1]} />

      {compact
        ? null
        : TASKS.map((k) => (
            <text key={k.route} className="dg-mono ha-route" data-ha-route={k.route} data-on={f.routes[k.route] ? "" : undefined} x={ROUTE_LABEL[k.route][0]} y={ROUTE_LABEL[k.route][1]} textAnchor="end" fontSize={12} fill="var(--lane-muted)">
              {k.channel}
            </text>
          ))}

      {scene.nodes.map((n) => (
        <g key={n.id}>
          <rect x={n.x} y={n.y} width={n.w} height={n.h} rx={z.cardR} fill="var(--lane-raised)" stroke="var(--lane-line)" />
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 22 : 32)} className="dg-mono" fontSize={compact ? 13 : 17} fontWeight={500} fill="var(--lane-ink)">
            {n.id}
          </text>
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 40 : 56)} className="dg-sans" fontSize={compact ? 11 : 14} fill="var(--lane-muted)">
            {compact ? n.product : `${n.product} · ${n.machine}`}
          </text>
          <text x={n.x + (compact ? 10 : 16)} y={n.y + (compact ? 58 : 78)} className="dg-mono" fontSize={compact ? 10.5 : 13} fill="var(--lane-muted)" data-ha-state={compact ? undefined : n.id}>
            {compact ? n.machine : f.states[n.id]}
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

/** Direction is labelled in words, not only by colour. */
function Legend({ scene, track, back }: { scene: Scene; track: Track; back: Track }) {
  const a = scene.routes[0];
  const t = tracks(scene, a);
  if (scene.compact) {
    const sx = t.sent[0][0] + 9;
    const rx = t.received[0][0] - 9;
    const sy = t.sent[0][1] + track.length * 0.3 + 4;
    const ry = t.received[0][1] + back.length * 0.12 + 4;
    return (
      <g className="dg-mono" fontSize={10.5}>
        <text x={sx} y={sy} fill="var(--lane-teal)">sent ↓</text>
        <text x={rx} y={ry} textAnchor="end" fill="var(--lane-orange)">↑ received</text>
      </g>
    );
  }
  return (
    <g className="dg-mono" fontSize={13}>
      <text x={t.sent[0][0] + track.length * 0.05} y={t.sent[0][1] - 13} fill="var(--lane-teal)">sent →</text>
      <text x={t.received[0][0] + back.length * 0.05} y={t.received[0][1] + 24} fill="var(--lane-orange)">← received</text>
    </g>
  );
}
