// Bake-off capture tool: poster frame, a SwiftShader recording of the live hero,
// and a rough frame-time sample. SwiftShader is a CPU WebGPU adapter: good for
// correctness and recordings, NOT for judging frame rate.
//
//   xvfb-run -a node site/experiments/tools/record.mjs <url> <outDir> [seconds=8]
//
// Headed under Xvfb on purpose: new-mode headless screenshots and videos do not
// include WebGPU canvases (the canvas renders, the capture is blank).
//
// Uses the Playwright 1.49.1 install (and its cached Chromium) in /tmp/acs-tools.
import { createRequire } from "node:module";
import { mkdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const require = createRequire("/tmp/acs-tools/package.json");
const { chromium } = require("playwright");
const [url, out, secs = "8"] = process.argv.slice(2);
if (!url || !out) throw new Error("usage: record.mjs <url> <outDir> [seconds]");
mkdirSync(out, { recursive: true });

// The full Chromium build (channel "chromium") in new headless mode exposes a
// SwiftShader WebGPU adapter; the default headless shell does not.
const GPU_ARGS = [
  "--enable-unsafe-webgpu",
  "--enable-features=Vulkan",
  "--use-angle=vulkan",
  "--use-vulkan=swiftshader",
  "--use-webgpu-adapter=swiftshader",
  "--disable-vulkan-surface",
];
const viewport = { width: 1440, height: 900 };

// 1. Poster frame: WebGPU forced off.
{
  const b = await chromium.launch({ headless: false, channel: "chromium", args: GPU_ARGS });
  const p = await b.newPage({ viewport });
  await p.goto(`${url}${url.includes("?") ? "&" : "?"}gpu=off`, { waitUntil: "load" });
  await p.waitForTimeout(500);
  await p.locator("[data-hero-stage]").first().screenshot({ path: join(out, "poster.png") });
  await b.close();
}

// 2. Live hero, recorded.
const b = await chromium.launch({ headless: false, channel: "chromium", args: GPU_ARGS });
const ctx = await b.newContext({ viewport, recordVideo: { dir: out, size: viewport } });
const p = await ctx.newPage();
p.on("console", (m) => console.log(`[page] ${m.text()}`));
await p.goto(url, { waitUntil: "load" });
await p.waitForSelector('[data-gpu="on"]', { timeout: 20000 }).catch(() => console.log("live layer never switched on"));
const frames = await p.evaluate(
  (ms) =>
    new Promise((resolve) => {
      const t = [];
      const t0 = performance.now();
      const tick = (now) => {
        t.push(now);
        if (now - t0 < ms) requestAnimationFrame(tick);
        else resolve(t);
      };
      requestAnimationFrame(tick);
    }),
  Number(secs) * 1000,
);
await p.locator("[data-hero-stage]").first().screenshot({ path: join(out, "live.png") });
const video = p.video();
await ctx.close();
await b.close();
if (video) renameSync(await video.path(), join(out, "recording.webm"));

const d = frames.slice(1).map((x, i) => x - frames[i]).sort((a, c) => a - c);
const q = (f) => d[Math.min(d.length - 1, Math.floor(d.length * f))]?.toFixed(1);
const report = { url, frames: d.length, medianMs: q(0.5), p95Ms: q(0.95), note: "rAF interval under SwiftShader; rough guide only" };
writeFileSync(join(out, "frames.json"), JSON.stringify(report, null, 2));
console.log(report);
