"use client";

// A full-bleed copy of components/hero/gpu-stage.tsx (shared, read-only): the
// same progressive enhancement (poster first, the GPU module only after first
// paint, only with WebGPU, never under reduced motion, paused off-screen and in
// hidden tabs, ?gpu=off forces the poster), but the stage fills its parent
// instead of an aspect-ratio box, and it starts the web with its own layout:
// the aspect band's (the same media queries the poster's CSS uses). Once live
// it also measures the copy block into design space (the field yields around
// it, less behind the headline) and turns a pointerdown on empty field into a
// neutral ring.
// The poster crossfades out once the live canvas has a frame.
import { useEffect, useRef, useState } from "react";
import type { HeroPalette } from "@/components/hero/gpu-stage";
import type { WebRenderer } from "./web";
import { BAND_QUERY, bandFor, cover, DESIGN, type Band } from "./web-bands";
import css from "./web.module.css";

// Copy of readPalette() from components/hero/gpu-stage.tsx: importing it would
// pull the shared stage module into this first-paint chunk.
const VARS: Record<keyof HeroPalette, string> = {
  lane: "--lane",
  laneRaised: "--lane-raised",
  laneLine: "--lane-line",
  laneInk: "--lane-ink",
  laneMuted: "--lane-muted",
  teal: "--lane-teal",
  orange: "--lane-orange",
};
function readPalette(el: Element): HeroPalette {
  const cs = getComputedStyle(el);
  const out = {} as HeroPalette;
  for (const [k, v] of Object.entries(VARS) as [keyof HeroPalette, string][]) {
    const hex = cs.getPropertyValue(v).trim().replace("#", "");
    out[k] = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255) as [number, number, number];
  }
  return out;
}

/** Pointerdowns here are the page's own controls and chrome, not the field. */
const NOT_FIELD = "a, button, input, header, [data-web-log]";

/** True on a line of the copy's text (a pointerdown there selects text); the block's empty space is field. */
function onText(copy: Element, x: number, y: number) {
  const r = document.createRange();
  for (const el of copy.querySelectorAll(".hero-title, .hero-sub")) {
    r.selectNodeContents(el);
    for (const b of r.getClientRects()) if (x >= b.left - 4 && x <= b.right + 4 && y >= b.top && y <= b.bottom) return true;
  }
  return false;
}
/** The copy rect grows by this much (CSS px) before the feather starts. */
const COPY_MARGIN = 16;

function useBand(variant: "wide" | "tall"): Band {
  const [band, setBand] = useState<Band>(variant === "tall" ? "tall" : "M");
  useEffect(() => {
    if (variant === "tall") return;
    const qs = [matchMedia(BAND_QUERY.M), matchMedia(BAND_QUERY.W)];
    const sync = () => setBand(bandFor((q) => matchMedia(q).matches));
    sync();
    qs.forEach((q) => q.addEventListener("change", sync));
    return () => qs.forEach((q) => q.removeEventListener("change", sync));
  }, [variant]);
  return band;
}

const load = () => import("./web");
const idle = (cb: () => void): number =>
  typeof window.requestIdleCallback === "function" ? window.requestIdleCallback(cb, { timeout: 1500 }) : (setTimeout(cb, 200) as unknown as number);

export function WebLive({ variant, poster }: { variant: "wide" | "tall"; poster: React.ReactNode }) {
  const stage = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [live, setLive] = useState(false);
  const band = useBand(variant);

  useEffect(() => {
    const el = stage.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    if (!("gpu" in navigator) || reduced.matches || new URLSearchParams(location.search).get("gpu") === "off") return;
    if (el.offsetParent === null) return; // only the variant that is displayed starts

    const design = DESIGN[band === "tall" ? "tall" : "wide"];
    const section = el.closest("section");
    let renderer: WebRenderer | null = null;
    let disposed = false;
    let onScreen = true;
    const sync = () => renderer?.setPaused(!onScreen || document.hidden);
    const start = async () => {
      try {
        const mod = await load();
        if (disposed) return;
        renderer = await mod.startWeb(cv, { band, palette: readPalette(el) });
        if (disposed) return renderer.dispose();
        sync();
        measure();
        section?.addEventListener("pointerdown", onPointer);
        section?.addEventListener("pointermove", onMove);
        section?.addEventListener("pointerleave", onLeave);
        setLive(true);
      } catch (err) {
        console.info("hero: keeping the poster frame", err);
      }
    };
    // Client px → design units, through the canvas's own cover mapping.
    const toDesign = (x: number, y: number) => {
      const r = cv.getBoundingClientRect();
      const m = cover(design, r.width, r.height);
      return [(x - r.left - m.x) / m.k, (y - r.top - m.y) / m.k] as const;
    };
    const copy = section?.querySelector("[data-web-copy]");
    const title = copy?.querySelector(".hero-title");
    const measure = () => {
      if (!renderer || !copy) return;
      const c = copy.getBoundingClientRect();
      const [x0, y0] = toDesign(c.left - COPY_MARGIN, c.top - COPY_MARGIN);
      const [x1, y1] = toDesign(c.right + COPY_MARGIN, c.bottom + COPY_MARGIN);
      // The headline's bottom: above it the tracks yield less (the reaches show behind the large type).
      const head = title ? toDesign(0, title.getBoundingClientRect().bottom)[1] : y0;
      renderer.setCopy([x0, y0, x1, y1, head]);
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    if (copy) ro.observe(copy);
    const onPointer = (e: PointerEvent) => {
      if (!renderer || (e.target instanceof Element && e.target.closest(NOT_FIELD))) return;
      if (copy && e.target instanceof Node && copy.contains(e.target) && onText(copy, e.clientX, e.clientY)) return;
      const [x, y] = toDesign(e.clientX, e.clientY);
      renderer.click(x, y);
    };
    const onMove = (e: PointerEvent) => {
      if (!renderer || e.pointerType === "touch") return;
      const [x, y] = toDesign(e.clientX, e.clientY);
      renderer.hover(x, y);
    };
    const onLeave = () => renderer?.leave();
    const handle = idle(() => void start());
    const io = new IntersectionObserver(([e]) => {
      onScreen = e.isIntersecting;
      sync();
    });
    io.observe(el);
    document.addEventListener("visibilitychange", sync);
    const onReduced = () => {
      if (reduced.matches) {
        section?.removeEventListener("pointerdown", onPointer);
        section?.removeEventListener("pointermove", onMove);
        section?.removeEventListener("pointerleave", onLeave);
        renderer?.dispose();
        renderer = null;
        setLive(false);
      }
    };
    reduced.addEventListener("change", onReduced);
    const theme = () => renderer?.setPalette(readPalette(el));
    const mo = new MutationObserver(theme);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => {
      disposed = true;
      if (typeof window.cancelIdleCallback === "function") window.cancelIdleCallback(handle);
      else clearTimeout(handle);
      io.disconnect();
      ro.disconnect();
      mo.disconnect();
      section?.removeEventListener("pointerdown", onPointer);
      section?.removeEventListener("pointermove", onMove);
      section?.removeEventListener("pointerleave", onLeave);
      document.removeEventListener("visibilitychange", sync);
      reduced.removeEventListener("change", onReduced);
      renderer?.dispose();
    };
  }, [band]);

  return (
    <div ref={stage} className={css.stage} data-gpu={live ? "on" : "off"}>
      <div className={css.stagePoster}>{poster}</div>
      <canvas ref={canvas} className={css.stageCanvas} data-hero-web="" aria-hidden="true" />
    </div>
  );
}

/**
 * Safety net for labels and place cards (poster and live share the overlay,
 * so this covers both, with or without a GPU): anything that would touch the
 * copy, the header or the log strip (+12 px) is hidden at this viewport.
 */
export function useLabelGuard(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const section = ref.current;
    if (!section) return;
    const check = () => {
      const keep = ["[data-web-copy]", "[data-web-log]", "header"]
        .map((q) => document.querySelector(q)?.getBoundingClientRect())
        .filter((r): r is DOMRect => !!r);
      section.querySelectorAll<SVGGraphicsElement>("[data-agent-label], [data-place]").forEach((label) => {
        label.style.visibility = "";
        const r = label.getBoundingClientRect();
        if (!r.width) return;
        const hit = keep.some((k) => r.left < k.right + 12 && r.right > k.left - 12 && r.top < k.bottom + 12 && r.bottom > k.top - 12);
        if (hit) label.style.visibility = "hidden";
      });
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(section);
    const copy = section.querySelector("[data-web-copy]");
    if (copy) ro.observe(copy);
    document.fonts?.ready.then(check);
    return () => ro.disconnect();
  }, [ref]);
}

/** The section's client half: runs the label guard. Children are server-rendered. */
export function WebSection({ children, className }: { children: React.ReactNode; className?: string }) {
  const ref = useRef<HTMLElement>(null);
  useLabelGuard(ref);
  return (
    <section ref={ref} aria-labelledby="hero-title" className={className} data-hero-stage>
      {children}
    </section>
  );
}

/** Pause control for the live web (WCAG 2.2.2); talks to whichever canvas is running. */
export function WebPause() {
  const [paused, setPaused] = useState(false);
  const toggle = (e: React.MouseEvent<HTMLButtonElement>) => {
    const next = !paused;
    setPaused(next);
    e.currentTarget.closest("section")?.querySelectorAll("canvas[data-hero-web]").forEach((c) => c.dispatchEvent(new CustomEvent("hero:pause", { detail: next })));
  };
  return (
    <button type="button" className={css.pause} onClick={toggle} aria-pressed={paused} aria-label={paused ? "Play the message animation" : "Pause the message animation"}>
      <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
        {paused ? <path d="M2.5 1.5 8.5 5l-6 3.5z" fill="currentColor" /> : <path d="M2.5 1.5h1.8v7H2.5zm3.2 0h1.8v7H5.7z" fill="currentColor" />}
      </svg>
      {paused ? "play" : "pause"}
    </button>
  );
}
