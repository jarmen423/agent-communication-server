// The poster: the tape frozen at T0, as server-rendered SVG. It is drawn from
// the same layout and the same `buildMarks` as the GPU layer, so the canvas's
// first frame lands exactly on it. Graphics only; the words live in overlay.tsx.
import { tilePath } from "@/components/hero/geometry";
import { KIND, T0, buildMarks, type Layout } from "./schedule";

const FILL = ["var(--lane-teal)", "var(--lane-orange)", "var(--lane-ink)", "var(--lane-muted)"];
const f = (n: number) => Math.round(n * 100) / 100;

/** Gradient stops approximating the shader's smoothstep fade. */
export function FadeStops({ reverse = false }: { reverse?: boolean }) {
  const stops = [0, 0.156, 0.5, 0.844, 1];
  return (
    <>
      {stops.map((v, i) => (
        <stop key={i} offset={i / 4} stopColor="#fff" stopOpacity={reverse ? stops[4 - i] : v} />
      ))}
    </>
  );
}

/** A mask that fades the tape in at x0 and out at x1 (or cuts it at `until`). */
export function TapeMask({ id, L, until }: { id: string; L: Layout; until?: number }) {
  return (
    <>
      <linearGradient id={`${id}-in`} gradientUnits="userSpaceOnUse" x1={L.x0} x2={L.x0 + L.fade} y1={0} y2={0}>
        <FadeStops />
      </linearGradient>
      <linearGradient id={`${id}-out`} gradientUnits="userSpaceOnUse" x1={L.x1 - L.fade} x2={L.x1} y1={0} y2={0}>
        <FadeStops reverse />
      </linearGradient>
      <mask id={id} maskUnits="userSpaceOnUse" x={0} y={0} width={L.width} height={L.height}>
        <rect x={L.x0} y={0} width={L.fade} height={L.height} fill={`url(#${id}-in)`} />
        <rect x={L.x0 + L.fade} y={0} width={(until ?? L.x1 - L.fade) - L.x0 - L.fade} height={L.height} fill="#fff" />
        {until === undefined ? <rect x={L.x1 - L.fade} y={0} width={L.fade} height={L.height} fill={`url(#${id}-out)`} /> : null}
      </mask>
    </>
  );
}

export function TapePoster({ L, id }: { L: Layout; id: string }) {
  const x = (t: number) => L.play - (T0 - t) * L.pps;
  const first = Math.ceil(T0 - (L.play - L.x0) / L.pps);
  const last = Math.floor(T0 + (L.x1 - L.play) / L.pps);
  const seconds = Array.from({ length: last - first + 1 }, (_, i) => first + i);
  const marks = buildMarks(T0, L, false);

  return (
    <svg viewBox={`0 0 ${L.width} ${L.height}`} className="h-full w-full" aria-hidden="true" preserveAspectRatio="xMidYMid meet">
      <defs>
        <TapeMask id={`${id}-fade`} L={L} />
      </defs>
      <g mask={`url(#${id}-fade)`} className="hc-poster-ink">
        {/* The paper: a 5 s grid and 1 s ticks, printed on the tape. */}
        <g strokeWidth={1}>
          {seconds.map((t) => {
            const major = t % 5 === 0;
            return (
              <g key={t}>
                {major ? <line x1={f(x(t))} x2={f(x(t))} y1={L.axisY} y2={L.bottom} stroke="var(--lane-line)" strokeOpacity={0.7} /> : null}
                <line x1={f(x(t))} x2={f(x(t))} y1={L.axisY} y2={L.axisY + (major ? 8 : 4)} stroke="var(--lane-muted)" strokeOpacity={major ? 0.8 : 0.55} />
              </g>
            );
          })}
          <line x1={L.x0} x2={L.x1} y1={L.axisY} y2={L.axisY} stroke="var(--lane-muted)" strokeOpacity={0.4} />
        </g>

        {/* The traces: drawn only as far as the pens at "now". */}
        {L.rows.map((r) =>
          r.hub ? (
            <g key={r.id} stroke="var(--lane-ink)" strokeOpacity={0.3} strokeWidth={1}>
              <line x1={L.x0} x2={L.play} y1={r.y - 3} y2={r.y - 3} />
              <line x1={L.x0} x2={L.play} y1={r.y + 3} y2={r.y + 3} />
            </g>
          ) : (
            <line key={r.id} x1={L.x0} x2={L.play} y1={r.y} y2={r.y} stroke="var(--lane-line)" strokeWidth={1.2} />
          ),
        )}

        {/* The ink: every envelope at T0. */}
        {marks.map((m, i) => {
          const fill = FILL[m.color];
          if (m.kind === KIND.dot) return <circle key={i} cx={f(m.cx)} cy={f(m.cy)} r={m.hw} fill={fill} fillOpacity={m.alpha} />;
          if (m.kind === KIND.sentTile || m.kind === KIND.recvTile) {
            const dir = m.kind === KIND.sentTile ? "sent" : "received";
            return <path key={i} d={tilePath(f(m.cx), f(m.cy), m.hw * 2, m.hh * 2, dir, Math.min(2.5, m.hh))} fill={fill} fillOpacity={m.alpha} />;
          }
          if (m.kind !== KIND.bar) return null;
          const w = Math.max(m.hw * 2, 1);
          const h = Math.max(m.hh * 2, 1);
          const r = Math.min(w, h) / 2;
          return <rect key={i} x={f(m.cx - w / 2)} y={f(m.cy - h / 2)} width={f(w)} height={f(h)} rx={f(r)} fill={fill} fillOpacity={m.alpha} />;
        })}
      </g>

      {/* Pens and playhead stay put: they are the recorder, not the paper. */}
      <g className="hc-poster-ink">
        <line x1={L.play} x2={L.play} y1={L.axisY - 10} y2={L.bottom + 6} stroke="var(--lane-ink)" strokeOpacity={0.42} strokeWidth={1} />
        {L.rows.map((r) =>
          r.hub ? (
            <g key={r.id} fill="var(--lane-ink)" fillOpacity={0.8}>
              <circle cx={L.play} cy={r.y - 3} r={1.8} />
              <circle cx={L.play} cy={r.y + 3} r={1.8} />
            </g>
          ) : (
            <circle key={r.id} cx={L.play} cy={r.y} r={2.4} fill="var(--lane-ink)" fillOpacity={0.85} />
          ),
        )}
      </g>
    </svg>
  );
}
