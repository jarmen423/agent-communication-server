"use client";

// Concept A's hero stage: both poster boxes (wide from md, tall below), each a
// <GpuStage> over its server-rendered poster, plus the visitor's pause switch.
import { useSyncExternalStore } from "react";
import { GpuStage } from "@/components/hero/gpu-stage";
import { TALL, WIDE } from "@/components/hero/geometry";
import { motion } from "./control";
import "./hero-a.css";

const load = () => import("./scene");

export function SignalBoxStage({ wide, tall }: { wide: React.ReactNode; tall: React.ReactNode }) {
  return (
    <>
      <div className="hero-poster hero-poster-wide ha-stage">
        <GpuStage scene={WIDE} poster={wide} load={load} />
        <PauseSwitch />
      </div>
      <div className="hero-poster hero-poster-tall ha-stage">
        <GpuStage scene={TALL} poster={tall} load={load} />
        <PauseSwitch />
      </div>
    </>
  );
}

const subscribe = (cb: () => void) => motion.subscribe(cb);
const get = () => motion.paused;

/** Shown only while the live layer runs (CSS keys it off data-gpu="on"). */
function PauseSwitch() {
  const paused = useSyncExternalStore(subscribe, get, () => false);
  return (
    <button
      type="button"
      className="ha-pause"
      data-paused={paused ? "" : undefined}
      aria-label={`${paused ? "Play" : "Pause"} the route diagram animation`}
      onClick={() => motion.set(!paused)}
    >
      <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
        {paused ? <path d="M2 1 9 5 2 9Z" fill="currentColor" /> : <path d="M2 1h2v8H2zM6 1h2v8H6z" fill="currentColor" />}
      </svg>
      <span className="ha-pause-text" aria-hidden="true">
        {paused ? "play" : "pause"}
      </span>
    </button>
  );
}
