"use client";
// The hub's centre, in the overlay's design space. Default: the mark's lens as
// translucent glass over its plate. Experiments for comparison, picked with
// ?hub=rings | orb | ledger:
//  - rings:  two tilted orbits of agent marks rotating in different planes
//  - orb:    a translucent glass sphere, slow light inside
//  - ledger: an append-only log of the hub's traffic, scrolling as it grows
// Reduced motion: the experiments hold still.
import { useEffect, useId, useRef, useState } from "react";
import { lensPath } from "@/components/hero/geometry";
import poster from "./posters/web.svg";

const SRC = (poster as { src: string }).src;
type Kind = "glass" | "rings" | "orb" | "ledger";
const KINDS: readonly Kind[] = ["glass", "rings", "orb", "ledger"];
type Hub = { cx: number; cy: number; size: number };
type Plate = { x: number; y: number; w: number; h: number; r: number };

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Coloured marks use their own symbol (ic-); the rest are single-colour (i-), drawn in ink. */
const COLOURED = new Set(["claude", "codex", "agy"]);
const markHref = (name: string) => `${SRC}#${COLOURED.has(name) ? "ic" : "i"}-${name}`;

export function HubCenter({ hub, plate }: { hub: Hub; plate: Plate }) {
  const [kind, setKind] = useState<Kind>("glass");
  useEffect(() => {
    const k = new URLSearchParams(location.search).get("hub") as Kind | null;
    if (k && KINDS.includes(k)) setKind(k);
  }, []);
  if (kind === "orb") return <Orb hub={hub} plate={plate} />;
  if (kind === "ledger") return <Ledger hub={hub} plate={plate} />;
  return (
    <>
      <PlateRect plate={plate} />
      <Glass hub={hub} />
      {kind === "rings" && <Rings hub={hub} plate={plate} />}
    </>
  );
}

function PlateRect({ plate }: { plate: Plate }) {
  return <rect x={plate.x} y={plate.y} width={plate.w} height={plate.h} rx={plate.r} fill="var(--lane-raised)" fillOpacity={0.45} stroke="var(--lane-line)" strokeWidth={1.5} />;
}

/** The lens as glass: a faint fill and a fine rim, so the tracks' junction shows through. */
function Glass({ hub }: { hub: Hub }) {
  return <path d={lensPath(hub.cx, hub.cy, hub.size)} fill="var(--lane-ink)" fillOpacity={0.14} stroke="var(--lane-ink)" strokeOpacity={0.55} strokeWidth={1.25} />;
}

/* ---------------------------------------------------------------- rings */

type Ring = { r: number; tilt: number; roll: number; speed: number; marks: string[] };

function Rings({ hub, plate }: { hub: Hub; plate: Plate }) {
  const R = plate.w * 0.68;
  const rings: Ring[] = [
    { r: R, tilt: 70, roll: -20, speed: 0.22, marks: ["claude", "codex", "cursor", "grok", "hermes"] },
    { r: R * 0.82, tilt: 64, roll: 38, speed: -0.3, marks: ["opencode", "kilo", "agy", "openai"] },
  ];
  const s = hub.size * 0.26;
  const refs = useRef<(SVGUseElement | null)[][]>(rings.map(() => []));
  useEffect(() => {
    const still = reducedMotion();
    let raf = 0;
    const t0 = performance.now();
    const place = (now: number) => {
      const t = still ? 0 : (now - t0) / 1000;
      rings.forEach((ring, k) => {
        const tilt = (ring.tilt * Math.PI) / 180;
        const roll = (ring.roll * Math.PI) / 180;
        ring.marks.forEach((_, i) => {
          const el = refs.current[k][i];
          if (!el) return;
          const a = (i / ring.marks.length) * Math.PI * 2 + t * ring.speed;
          // Circle in its own plane, tipped about x by `tilt`, then rolled about the view axis.
          const x = ring.r * Math.cos(a);
          const y0 = ring.r * Math.sin(a);
          const y = y0 * Math.cos(tilt);
          const z = y0 * Math.sin(tilt); // > 0: towards the viewer
          const px = hub.cx + x * Math.cos(roll) - y * Math.sin(roll);
          const py = hub.cy + x * Math.sin(roll) + y * Math.cos(roll);
          const depth = z / ring.r; // -1 back … 1 front
          const k2 = s * (0.7 + 0.3 * (depth + 1) / 2); // nearer marks a little larger
          el.setAttribute("x", String(px - k2 / 2));
          el.setAttribute("y", String(py - k2 / 2));
          el.setAttribute("width", String(k2));
          el.setAttribute("height", String(k2));
          el.setAttribute("opacity", String(0.3 + 0.7 * (depth + 1) * 0.5));
        });
      });
      if (!still) raf = requestAnimationFrame(place);
    };
    raf = requestAnimationFrame(place);
    return () => cancelAnimationFrame(raf);
    // The layout is static per band.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <g aria-hidden="true">
      {rings.map((ring, k) => (
        <ellipse
          key={`o${k}`}
          cx={hub.cx}
          cy={hub.cy}
          rx={ring.r}
          ry={ring.r * Math.cos((ring.tilt * Math.PI) / 180)}
          transform={`rotate(${ring.roll} ${hub.cx} ${hub.cy})`}
          fill="none"
          stroke="var(--lane-ink)"
          strokeOpacity={0.16}
          strokeWidth={1.2}
        />
      ))}
      {rings.map((ring, k) =>
        ring.marks.map((m, i) => (
          <use
            key={`${k}-${m}`}
            ref={(el) => {
              refs.current[k][i] = el;
            }}
            href={markHref(m)}
            fill="var(--lane-ink)"
            x={hub.cx}
            y={hub.cy}
            width={0}
            height={0}
          />
        )),
      )}
    </g>
  );
}

/* ---------------------------------------------------------------- orb */

function Orb({ hub, plate }: { hub: Hub; plate: Plate }) {
  const id = useId().replace(/:/g, "");
  const r = plate.w * 0.46;
  const still = useRef(false);
  const [, force] = useState(0);
  useEffect(() => {
    still.current = reducedMotion();
    force(1);
  }, []);
  const spin = (sec: number, rev = false) =>
    still.current ? undefined : <animateTransform attributeName="transform" type="rotate" from={`${rev ? 360 : 0} ${hub.cx} ${hub.cy}`} to={`${rev ? 0 : 360} ${hub.cx} ${hub.cy}`} dur={`${sec}s`} repeatCount="indefinite" />;
  const pulse = (sec: number, lo: number, hi: number, delay = 0) =>
    still.current ? undefined : <animate attributeName="opacity" values={`${lo};${hi};${lo}`} dur={`${sec}s`} begin={`${delay}s`} repeatCount="indefinite" />;
  return (
    <g aria-hidden="true">
      <defs>
        {/* Fresnel: clear in the middle, brighter towards the rim. */}
        <radialGradient id={`${id}-rim`} cx="50%" cy="50%" r="50%">
          <stop offset="0.55" stopColor="#eef0ea" stopOpacity="0.02" />
          <stop offset="0.86" stopColor="#eef0ea" stopOpacity="0.1" />
          <stop offset="1" stopColor="#eef0ea" stopOpacity="0.38" />
        </radialGradient>
        <radialGradient id={`${id}-hi`} cx="50%" cy="50%" r="50%">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0.55" />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0" />
        </radialGradient>
        <radialGradient id={`${id}-teal`} cx="50%" cy="50%" r="50%">
          <stop offset="0" stopColor="#3cb8ab" stopOpacity="0.55" />
          <stop offset="1" stopColor="#3cb8ab" stopOpacity="0" />
        </radialGradient>
        <radialGradient id={`${id}-orange`} cx="50%" cy="50%" r="50%">
          <stop offset="0" stopColor="#f5a742" stopOpacity="0.5" />
          <stop offset="1" stopColor="#f5a742" stopOpacity="0" />
        </radialGradient>
        <clipPath id={`${id}-clip`}>
          <circle cx={hub.cx} cy={hub.cy} r={r} />
        </clipPath>
      </defs>
      {/* Back glass: a faint dark body so the tracks dim a little behind it. */}
      <circle cx={hub.cx} cy={hub.cy} r={r} fill="#101317" fillOpacity={0.35} />
      <g clipPath={`url(#${id}-clip)`}>
        {/* Light drifting inside: sent (teal) and received (orange), slowly orbiting and breathing. */}
        <g>
          {spin(22)}
          <circle cx={hub.cx - r * 0.35} cy={hub.cy + r * 0.2} r={r * 0.75} fill={`url(#${id}-teal)`}>
            {pulse(5.5, 0.55, 1)}
          </circle>
        </g>
        <g>
          {spin(30, true)}
          <circle cx={hub.cx + r * 0.38} cy={hub.cy - r * 0.15} r={r * 0.7} fill={`url(#${id}-orange)`}>
            {pulse(6.5, 0.45, 0.95, 1.8)}
          </circle>
        </g>
        {/* A slow caustic band. */}
        <g>
          {spin(40)}
          <ellipse cx={hub.cx} cy={hub.cy + r * 0.1} rx={r * 0.95} ry={r * 0.22} fill="none" stroke="#eef0ea" strokeOpacity={0.12} strokeWidth={r * 0.06} />
        </g>
        <path d={lensPath(hub.cx, hub.cy, hub.size * 0.62)} fill="#eef0ea" fillOpacity={0.2} />
      </g>
      <circle cx={hub.cx} cy={hub.cy} r={r} fill={`url(#${id}-rim)`} />
      <circle cx={hub.cx} cy={hub.cy} r={r} fill="none" stroke="#eef0ea" strokeOpacity={0.32} strokeWidth={1.2} />
      {/* Specular highlight, upper left. */}
      <ellipse cx={hub.cx - r * 0.38} cy={hub.cy - r * 0.45} rx={r * 0.32} ry={r * 0.18} transform={`rotate(-30 ${hub.cx - r * 0.38} ${hub.cy - r * 0.45})`} fill={`url(#${id}-hi)`} />
    </g>
  );
}

/* ---------------------------------------------------------------- ledger */

const AGENTS = ["me", "claude-1", "codex-5", "cursor-2", "grok-4", "hermes-3", "opencode-6", "kilo-2", "agy-1", "claude-web-1", "chatgpt-web-1", "ci-3"];
const PROMPTS = ["fix the failing router test", "add a changelog entry", "review the presence handler", "write tests for the merge gate", "update the docs index", "profile hub startup", "bump async-nats"];
const STEPS = ["reading files", "editing", "cargo test", "writing summary"];
const RESULTS = ["2 files changed", "tests pass", "PR ready for review", "no issues found", "entry added"];

type Line = { id: number; t: string; kind: "task" | "status" | "progress" | "result"; text: string };

/** A small deterministic PRNG so server and first client render agree. */
function prng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

function makeLine(rand: () => number, id: number, clock: number): Line {
  const pick = <T,>(a: readonly T[]) => a[Math.floor(rand() * a.length)];
  const t = new Date(Date.UTC(2026, 0, 1, 12, 4, 0) + clock).toISOString().slice(11, 19);
  const task = Math.floor(rand() * 0xffff).toString(16).padStart(4, "0");
  const r = rand();
  const from = pick(AGENTS);
  let to = pick(AGENTS);
  if (to === from) to = "codex-1";
  const prompt = pick(PROMPTS);
  const short = prompt.length > 20 ? `${prompt.slice(0, 19)}…` : prompt;
  if (r < 0.3) return { id, t, kind: "task", text: `${from} → ${to} "${short}"` };
  if (r < 0.5) return { id, t, kind: "status", text: `${to} task.${task} working` };
  if (r < 0.78) return { id, t, kind: "progress", text: `${to} task.${task} ${pick(STEPS)}` };
  return { id, t, kind: "result", text: `${to} task.${task} done · ${pick(RESULTS)}` };
}

const LEDGER_TONE: Record<Line["kind"], string> = { task: "var(--lane-teal)", status: "var(--lane-orange)", progress: "var(--lane-muted)", result: "var(--lane-orange)" };

function Ledger({ hub, plate }: { hub: Hub; plate: Plate }) {
  const id = useId().replace(/:/g, "");
  const w = plate.w * 1.8;
  const h = plate.h * 1.4;
  // Off-centre to the left: the places' cards sit just right of the hub.
  const x = hub.cx + plate.w * 0.45 - w;
  const y = plate.y + plate.h - h; // grows upward from the plate's bottom, above the hub label
  const fs = Math.max(12, plate.w * 0.075);
  const lh = fs * 1.45;
  const pad = fs * 0.9;
  const rows = Math.floor((h - pad * 2 - lh) / lh) + 1;
  const rand = useRef(prng(7));
  const clock = useRef(0);
  const next = useRef(0);
  const [lines, setLines] = useState<Line[]>(() => {
    const out: Line[] = [];
    for (let i = 0; i < rows + 1; i++) out.push(makeLine(rand.current, next.current++, (clock.current += 900)));
    return out;
  });
  const [shift, setShift] = useState<{ y: number; anim: boolean }>({ y: 0, anim: false });
  useEffect(() => {
    if (reducedMotion()) return;
    const tick = () => {
      // Append one line below, then glide everything up by a row.
      setLines((ls) => [...ls.slice(-(rows + 1)), makeLine(rand.current, next.current++, (clock.current += 400 + Math.floor(rand.current() * 1400)))]);
      setShift({ y: lh, anim: false });
      requestAnimationFrame(() => requestAnimationFrame(() => setShift({ y: 0, anim: true })));
      timer = window.setTimeout(tick, 650 + rand.current() * 900);
    };
    let timer = window.setTimeout(tick, 900);
    return () => clearTimeout(timer);
  }, [rows, lh]);
  const shown = lines.slice(-(rows + 1));
  return (
    <g aria-hidden="true" className="dg-mono">
      <defs>
        <clipPath id={`${id}-clip`}>
          <rect x={x} y={y} width={w} height={h} rx={plate.r} />
        </clipPath>
        <linearGradient id={`${id}-fade`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#191d23" stopOpacity="0.95" />
          <stop offset="0.35" stopColor="#191d23" stopOpacity="0" />
        </linearGradient>
      </defs>
      <rect x={x} y={y} width={w} height={h} rx={plate.r} fill="var(--lane-raised)" fillOpacity={0.72} stroke="var(--lane-line)" strokeWidth={1.5} />
      <g clipPath={`url(#${id}-clip)`}>
        <g style={{ transform: `translateY(${shift.y}px)`, transition: shift.anim ? "transform 420ms cubic-bezier(.2,.7,.2,1)" : "none" }}>
          {shown.map((l, i) => {
            const ly = y + h - pad - (shown.length - 1 - i) * lh;
            return (
              <text key={l.id} x={x + pad} y={ly} fontSize={fs} fill="var(--lane-muted)">
                <tspan fill="var(--lane-muted)" fillOpacity={0.6}>{l.t} </tspan>
                <tspan fill={LEDGER_TONE[l.kind]}>{l.kind.padEnd(9, " ")}</tspan>
                <tspan>{l.text}</tspan>
              </text>
            );
          })}
        </g>
        <rect x={x} y={y} width={w} height={h} fill={`url(#${id}-fade)`} />
      </g>
      <text x={x + pad} y={y + pad + fs * 0.4} fontSize={fs * 0.85} fill="var(--lane-ink)" fillOpacity={0.8}>
        ledger · append-only
      </text>
    </g>
  );
}
