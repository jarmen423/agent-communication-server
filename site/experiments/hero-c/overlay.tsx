// The words, above the canvas and always in the DOM: row labels, the hub's
// lens, the legend, "now", and two strips that ride with the tape (clock ticks
// and channel names). scene.ts slides the strips; without a GPU they stay at T0.
import { lensPath, tilePath } from "@/components/hero/geometry";
import { TapeMask } from "./poster";
import { T0, tapeLabels, type Layout, type TapeLabel } from "./schedule";

/** Fixed label pools: the GPU module rewrites these slots, never adds nodes. */
const TICK_SLOTS = 10;
const EVENT_SLOTS = 10;
const TONE: Record<TapeLabel["tone"], string> = { muted: "var(--lane-muted)", orange: "var(--lane-orange)", ink: "var(--lane-ink)" };
const ADVANCE = 0.6; // IBM Plex Mono advance width, in em

function Slots({ labels, count, size }: { labels: TapeLabel[]; count: number; size: number }) {
  return (
    <>
      {Array.from({ length: count }, (_, i) => {
        const l = labels[i];
        return (
          <text key={i} x={l?.x ?? 0} y={l?.y ?? 0} fill={TONE[l?.tone ?? "muted"]} fontSize={size} textAnchor={l?.anchor ?? "start"}>
            {l?.text ?? ""}
          </text>
        );
      })}
    </>
  );
}

function SentFlag({ x, y, h }: { x: number; y: number; h: number }) {
  return (
    <g fill="var(--lane-teal)">
      <rect x={x - 1} y={y - h} width={2} height={h} rx={1} />
      <path d={tilePath(x + 3.5, y - h + 3.5, 9, 7, "sent", 2.5)} />
    </g>
  );
}

function ReceivedFlag({ x, y, h }: { x: number; y: number; h: number }) {
  return (
    <g fill="var(--lane-orange)">
      <rect x={x - 1} y={y - h} width={2} height={h} rx={1} />
      <path d={tilePath(x - 3.5, y - h + 3.5, 9, 7, "received", 2.5)} />
    </g>
  );
}

function Legend({ L }: { L: Layout }) {
  const size = L.compact ? 10 : 11.5;
  const y = L.compact ? 430 : 426;
  const w = (s: string) => s.length * size * ADVANCE;
  let x = L.compact ? 10 : L.x0;
  const gap = L.compact ? 14 : 22;
  const items: React.ReactNode[] = [];
  const word = (s: string, fill: string) => {
    // SVG drops leading spaces in a text node: advance past them instead.
    const text = s.trimStart();
    x += w(s.slice(0, s.length - text.length));
    items.push(<text key={`w${x}`} x={x} y={y} fill={fill} fontSize={size}>{text}</text>);
    x += w(text);
  };
  items.push(<SentFlag key="sent" x={x + 1} y={y + 1} h={13} />);
  x += 16;
  word("sent", "var(--lane-teal)");
  x += gap;
  items.push(<rect key="tick" x={x} y={y - 6} width={1.5} height={7} rx={0.75} fill="var(--lane-orange)" />);
  x += 8;
  word("received", "var(--lane-orange)");
  word(L.compact ? " progress" : " · status, events", "var(--lane-muted)");
  x += gap;
  items.push(<ReceivedFlag key="result" x={x + 9} y={y + 1} h={17} />);
  x += 16;
  word(L.compact ? "result" : "received", L.compact ? "var(--lane-muted)" : "var(--lane-orange)");
  if (!L.compact) word(" · the one result", "var(--lane-muted)");
  return <g>{items}</g>;
}

function RowLabel({ L, row }: { L: Layout; row: Layout["rows"][number] }) {
  if (!L.compact) {
    const y = row.y + 4.5;
    if (row.hub) {
      return (
        <g>
          <path d={lensPath(31, row.y, 15)} fill="var(--lane-ink)" />
          <text x={46} y={y} fontSize={13} fontWeight={500} fill="var(--lane-ink)">hub-server</text>
          <text x={46} y={y + 17} fontSize={11.5} fill="var(--lane-muted)">routes by meta.to</text>
        </g>
      );
    }
    return (
      <text x={24} y={y} fontSize={13}>
        <tspan fill="var(--lane-ink)" fontWeight={500}>{row.id}</tspan>
        <tspan fill="var(--lane-muted)"> · {row.machine}</tspan>
      </text>
    );
  }
  // Phones have no gutter: the label sits under its trace on a backing, like a label on the recorder's glass.
  const size = 10.5;
  const text = row.hub ? "hub-server" : `${row.id} · ${row.machine}`;
  const x = row.hub ? 24 : 10;
  const y = row.y + 18;
  return (
    <g>
      <rect x={x - 6 - (row.hub ? 14 : 0)} y={y - 11} width={text.length * size * ADVANCE + 12 + (row.hub ? 14 : 0)} height={16} rx={4} fill="var(--lane)" fillOpacity={0.92} />
      {row.hub ? <path d={lensPath(15, y - 3.5, 11)} fill="var(--lane-ink)" /> : null}
      <text x={x} y={y} fontSize={size}>
        <tspan fill="var(--lane-ink)" fontWeight={500}>{row.hub ? text : row.id}</tspan>
        {row.hub ? null : <tspan fill="var(--lane-muted)"> · {row.machine}</tspan>}
      </text>
    </g>
  );
}

export function TapeOverlay({ L, id }: { L: Layout; id: string }) {
  const { ticks, events } = tapeLabels(T0, L);
  return (
    <svg viewBox={`0 0 ${L.width} ${L.height}`} className="hc-overlay dg-mono" aria-hidden="true" preserveAspectRatio="xMidYMid meet">
      <defs>
        <TapeMask id={`${id}-ticks`} L={L} />
        {/* Labels run up to ~100 units right of their mark: fade them over that much more, so none outlives its ink. */}
        <TapeMask id={`${id}-written`} L={{ ...L, fade: L.fade + 100 }} until={L.play} />
      </defs>

      {L.compact ? null : (
        <text x={24} y={L.axisY - 12} fontSize={12} fill="var(--lane-muted)">hub-watch --all</text>
      )}
      <g mask={`url(#${id}-ticks)`}>
        <g data-hc-ride="ticks">
          <Slots labels={ticks} count={TICK_SLOTS} size={L.compact ? 10 : 11.5} />
        </g>
      </g>
      {/* Channel names are printed as the paper passes the pen: clipped at "now". */}
      <g mask={`url(#${id}-written)`}>
        <g data-hc-ride="events">
          <Slots labels={events} count={EVENT_SLOTS} size={L.compact ? 10 : 11} />
        </g>
      </g>

      {L.rows.map((r) => (
        <RowLabel key={r.id} L={L} row={r} />
      ))}
      <text x={L.play} y={L.bottom + 22} textAnchor="middle" fontSize={L.compact ? 10.5 : 12} fill="var(--lane-ink)">now</text>
      <Legend L={L} />
    </svg>
  );
}
