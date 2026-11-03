"use client";

// A full-bleed copy of components/hero/gpu-stage.tsx (shared, read-only): the
// same progressive enhancement (poster first, the GPU module only after first
// paint, only with WebGPU, never under reduced motion, paused off-screen and in
// hidden tabs, ?gpu=off forces the poster), but the stage fills its parent
// instead of an aspect-ratio box, and it starts the web with its own layout.
// The poster crossfades out once the live canvas has a frame.
import { useEffect, useRef, useState } from "react";
import { readPalette, type HeroRenderer } from "@/components/hero/gpu-stage";
import { WEB } from "./web-model";
import css from "./web.module.css";

const load = () => import("./web");
const idle = (cb: () => void): number =>
  typeof window.requestIdleCallback === "function" ? window.requestIdleCallback(cb, { timeout: 1500 }) : (setTimeout(cb, 200) as unknown as number);

export function WebLive({ variant, poster }: { variant: "wide" | "tall"; poster: React.ReactNode }) {
  const stage = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [live, setLive] = useState(false);

  useEffect(() => {
    const el = stage.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    if (!("gpu" in navigator) || reduced.matches || new URLSearchParams(location.search).get("gpu") === "off") return;
    if (el.offsetParent === null) return; // only the variant that is displayed starts

    let renderer: HeroRenderer | null = null;
    let disposed = false;
    let onScreen = true;
    const sync = () => renderer?.setPaused(!onScreen || document.hidden);
    const start = async () => {
      try {
        const mod = await load();
        if (disposed) return;
        renderer = await mod.startWeb(cv, { layout: WEB[variant], palette: readPalette(el) });
        if (disposed) return renderer.dispose();
        sync();
        setLive(true);
      } catch (err) {
        console.info("hero: keeping the poster frame", err);
      }
    };
    const handle = idle(() => void start());
    const io = new IntersectionObserver(([e]) => {
      onScreen = e.isIntersecting;
      sync();
    });
    io.observe(el);
    document.addEventListener("visibilitychange", sync);
    const onReduced = () => {
      if (reduced.matches) {
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
      mo.disconnect();
      document.removeEventListener("visibilitychange", sync);
      reduced.removeEventListener("change", onReduced);
      renderer?.dispose();
    };
  }, [variant]);

  return (
    <div ref={stage} className={css.stage} data-gpu={live ? "on" : "off"}>
      <div className={css.stagePoster}>{poster}</div>
      <canvas ref={canvas} className={css.stageCanvas} data-hero-web="" aria-hidden="true" />
    </div>
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
