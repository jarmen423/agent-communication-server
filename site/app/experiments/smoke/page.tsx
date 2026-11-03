"use client";
import { GpuStage } from "@/components/hero/gpu-stage";
import { WIDE } from "@/components/hero/geometry";
import { Hero } from "@/components/hero/hero";
import { RouteDiagram } from "@/components/hero/route-diagram";

const load = () => import("@/experiments/smoke/scene");
export default function Smoke() {
  return <main><Hero stage={<div className="hero-poster-wide"><GpuStage scene={WIDE} poster={<RouteDiagram scene={WIDE} className="h-full w-full" />} load={load} /></div>} /></main>;
}
