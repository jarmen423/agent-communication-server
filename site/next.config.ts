import path from "node:path";
import type { NextConfig } from "next";
import { wgslTurbopackRule } from "@vgpu/wgsl/next";

// The docs are read from ../docs and ../README.md at build time, so the
// repository root is the tracing and Turbopack root.
const repoRoot = path.resolve(process.cwd(), "..");

const nextConfig: NextConfig = {
  // STATIC_EXPORT=1 writes a plain static site to out/ (Cloudflare static assets).
  // Every route is prerendered, so nothing is lost.
  // Own build dir, so an export never clobbers the .next a running `next start` uses.
  ...(process.env.STATIC_EXPORT === "1" ? { output: "export" as const, distDir: ".next-export" } : {}),
  reactStrictMode: true,
  poweredByHeader: false,
  turbopack: {
    root: repoRoot,
    rules: {
      "*.wgsl": wgslTurbopackRule({ minify: true }),
    },
  },
  outputFileTracingRoot: repoRoot,
};

export default nextConfig;
