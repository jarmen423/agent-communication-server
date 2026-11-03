"use client";

// The client half of the stage: GpuStage (poster first, GPU after first paint)
// under the shared annotation overlay, plus a pause control for the live layer
// (WCAG 2.2.2: motion that runs longer than 5 s needs a way to stop it).
import { useRef, useState } from "react";
import { GpuStage } from "@/components/hero/gpu-stage";
import { TALL, WIDE } from "@/components/hero/geometry";
import css from "./hero-b.module.css";

const load = () => import("./scene");

export function ExposureLive({ variant, poster, overlay }: { variant: "wide" | "tall"; poster: React.ReactNode; overlay: React.ReactNode }) {
  const frame = useRef<HTMLDivElement>(null);
  const [paused, setPaused] = useState(false);
  const toggle = () => {
    const next = !paused;
    setPaused(next);
    frame.current?.querySelector("canvas")?.dispatchEvent(new CustomEvent("hero:pause", { detail: next }));
  };
  return (
    <div ref={frame} className={css.frame}>
      <GpuStage scene={variant === "wide" ? WIDE : TALL} poster={poster} load={load} />
      {overlay}
      <button type="button" className={css.pause} onClick={toggle} aria-pressed={paused} aria-label={paused ? "Play the message animation" : "Pause the message animation"}>
        <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
          {paused ? <path d="M2.5 1.5 8.5 5l-6 3.5z" fill="currentColor" /> : <path d="M2.5 1.5h1.8v7H2.5zm3.2 0h1.8v7H5.7z" fill="currentColor" />}
        </svg>
        {paused ? "play" : "pause"}
      </button>
    </div>
  );
}
