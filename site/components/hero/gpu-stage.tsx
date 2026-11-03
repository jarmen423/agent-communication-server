"use client";

// Progressive enhancement for the hero's GPU layer. The poster (server-rendered,
// same box) is always there. The GPU module loads only after first paint, only
// where WebGPU exists, never under prefers-reduced-motion, and renders only while
// the stage is on screen and the tab is visible. ?gpu=off forces the poster.
import { useEffect, useRef, useState } from "react";
import type { Scene } from "./geometry";

/** Brand lane colours as linear-ish 0..1 RGB, read from the CSS variables. */
export type HeroPalette = Record<"lane" | "laneRaised" | "laneLine" | "laneInk" | "laneMuted" | "teal" | "orange", [number, number, number]>;

export type HeroRenderer = {
  /** Stop or restart the frame loop (off-screen, hidden tab). */
  setPaused(paused: boolean): void;
  /** The theme changed: new lane colours. */
  setPalette(palette: HeroPalette): void;
  dispose(): void;
};

export type HeroModule = {
  /** Resolves once the first frame is on the canvas; rejects (or throws) to keep the poster. */
  startHero(canvas: HTMLCanvasElement, opts: { scene: Scene; palette: HeroPalette }): Promise<HeroRenderer>;
};

const VARS: Record<keyof HeroPalette, string> = {
  lane: "--lane",
  laneRaised: "--lane-raised",
  laneLine: "--lane-line",
  laneInk: "--lane-ink",
  laneMuted: "--lane-muted",
  teal: "--lane-teal",
  orange: "--lane-orange",
};

export function readPalette(el: Element): HeroPalette {
  const cs = getComputedStyle(el);
  const out = {} as HeroPalette;
  for (const [k, v] of Object.entries(VARS) as [keyof HeroPalette, string][]) {
    const hex = cs.getPropertyValue(v).trim().replace("#", "");
    out[k] = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255) as [number, number, number];
  }
  return out;
}

const idle = (cb: () => void): number =>
  typeof window.requestIdleCallback === "function"
    ? window.requestIdleCallback(cb, { timeout: 1500 })
    : (setTimeout(cb, 200) as unknown as number);

export function GpuStage({
  poster,
  scene,
  load,
  label,
}: {
  poster: React.ReactNode;
  scene: Scene;
  load: () => Promise<HeroModule>;
  /** Accessible description of the live layer (the poster carries its own). */
  label?: string;
}) {
  const stage = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [live, setLive] = useState(false);

  useEffect(() => {
    const el = stage.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    if (!("gpu" in navigator) || reduced.matches || new URLSearchParams(location.search).get("gpu") === "off") return;
    // Only the variant (wide or tall) that is actually displayed starts.
    if (el.offsetParent === null) return;

    let renderer: HeroRenderer | null = null;
    let disposed = false;
    let onScreen = true;
    const sync = () => renderer?.setPaused(!onScreen || document.hidden);

    const start = async () => {
      try {
        const mod = await load();
        if (disposed) return;
        renderer = await mod.startHero(cv, { scene, palette: readPalette(el) });
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
    const scheme = matchMedia("(prefers-color-scheme: dark)");
    scheme.addEventListener("change", theme);

    return () => {
      disposed = true;
      if (typeof window.cancelIdleCallback === "function") window.cancelIdleCallback(handle);
      else clearTimeout(handle);
      io.disconnect();
      mo.disconnect();
      document.removeEventListener("visibilitychange", sync);
      reduced.removeEventListener("change", onReduced);
      scheme.removeEventListener("change", theme);
      renderer?.dispose();
    };
  }, [load, scene]);

  return (
    <div ref={stage} className="gpu-stage" data-gpu={live ? "on" : "off"} style={{ aspectRatio: `${scene.width} / ${scene.height}` }}>
      <div className="gpu-stage-poster">{poster}</div>
      <canvas ref={canvas} className="gpu-stage-canvas" aria-hidden={label ? undefined : true} aria-label={label} role={label ? "img" : undefined} />
    </div>
  );
}
