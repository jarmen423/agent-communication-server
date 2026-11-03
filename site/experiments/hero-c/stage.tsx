"use client";

// Client wrapper for one stage (wide or tall): GpuStage with our poster, the
// label overlay above the canvas, and the pause control the moving tape needs
// (WCAG 2.2.2). The poster and overlay arrive server-rendered as props.
import { useRef, useState } from "react";
import { GpuStage } from "@/components/hero/gpu-stage";
import { TALL, WIDE } from "@/components/hero/geometry";
import "./hero-c.css";

const load = () => import("./scene");

export function StripStage({
  variant,
  poster,
  overlay,
  label,
}: {
  variant: "wide" | "tall";
  poster: React.ReactNode;
  overlay: React.ReactNode;
  label: string;
}) {
  const root = useRef<HTMLDivElement>(null);
  const [paused, setPaused] = useState(false);
  const toggle = () => {
    const next = !paused;
    setPaused(next);
    root.current?.setAttribute("data-paused", String(next));
    root.current?.dispatchEvent(new CustomEvent("hc-pause", { detail: next }));
  };
  return (
    <div ref={root} className="hc-stage" data-hc-stage data-paused="false">
      <div role="img" aria-label={label}>
        <GpuStage scene={variant === "wide" ? WIDE : TALL} poster={poster} load={load} />
        {overlay}
      </div>
      <button type="button" className="hc-pause" onClick={toggle} aria-pressed={paused}>
        <span aria-hidden="true" className="hc-pause-icon" data-state={paused ? "paused" : "playing"} />
        {paused ? "Play tape" : "Pause tape"}
      </button>
    </div>
  );
}
