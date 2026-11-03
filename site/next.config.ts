import path from "node:path";
import type { NextConfig } from "next";
import { wgslTurbopackRule } from "@vgpu/wgsl/next";

// The docs are read from ../docs and ../README.md at build time, so the
// repository root is the tracing and Turbopack root.
const repoRoot = path.resolve(process.cwd(), "..");

const nextConfig: NextConfig = {
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
