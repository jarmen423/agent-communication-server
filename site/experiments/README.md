# Hero bake-off (scratch, delete after the pick)

Three concepts for the homepage hero's GPU layer, built in parallel against the
same slot. Each concept owns exactly two paths and nothing else:

| Concept | Owns | Dev port |
|---|---|---|
| A. Signal box | `site/experiments/hero-a/**`, `site/app/experiments/hero-a/**` | 3001 |
| B. Long exposure | `site/experiments/hero-b/**`, `site/app/experiments/hero-b/**` | 3002 |
| C. Strip chart | `site/experiments/hero-c/**`, `site/app/experiments/hero-c/**` | 3003 |

Port 3000 is the owner's tunnel and is reserved for the integration branch.
Bind dev servers to 127.0.0.1 (`npx next dev -H 127.0.0.1 --port 300X`):
another process already listens on :3000 on the tailscale interface.

## The slot

- The page is the real homepage hero: `site/app/experiments/hero-<x>/page.tsx`
  renders `<SiteHeader/>` and `<Hero stage={…} />` from `components/hero/hero.tsx`.
- `stage` must keep the poster's box: aspect **1200:440 from `md` (768px) up,
  360:440 below**, inside the lane band. Wrap each in the existing
  `hero-poster hero-poster-wide` / `hero-poster hero-poster-tall` classes.
- Use `<GpuStage poster={…} scene={WIDE|TALL} load={() => import("…")} />`
  from `components/hero/gpu-stage.tsx`. It handles first paint, `navigator.gpu`
  detection, lazy import, `prefers-reduced-motion`, off-screen and hidden-tab
  pausing, theme changes and `?gpu=off`. Your module exports
  `startHero(canvas, { scene, palette })` returning `{ setPaused, setPalette, dispose }`.
- The poster is server-rendered (SVG or CSS, no client JS), the same size, and
  tells the same story without a GPU. Labels stay in the DOM/SVG, not the shader.

## Shared code is read-only

Import from `components/hero/*`, `content/*` and `app/*.css` freely; never edit
them. If you need a change there, copy what you need into your own folder and
say so in your report.

## Capture

    xvfb-run -a node site/experiments/tools/record.mjs http://127.0.0.1:300X/experiments/hero-<x> site/experiments/hero-<x>/capture

writes `poster.png` (WebGPU off), `live.png`, `recording.webm` and
`frames.json` (SwiftShader rAF intervals: a rough guide, not a frame rate).

Run it under `xvfb-run -a`: new-mode headless captures leave WebGPU canvases
blank even though they render. The tool launches the full Chromium build
(`channel: "chromium"`), which exposes a SwiftShader adapter; the default
headless shell has no WebGPU adapter at all.

## Lessons already learned (smoke test)

- `site/experiments/smoke/` + `/experiments/smoke` is a working reference for
  the module contract: a `.wgsl` import, `effect()`, `frameLoop()`, pause,
  palette and dispose. Copy patterns from it; don't edit it.
- `npx vgpu check` validates in Node (Dawn). Chrome's compiler is stricter:
  `smoothstep(0.02, 0.0, x)` passed `vgpu check` and failed in the browser
  ("low not less than high"). Always read the browser console too.
- Resolve `startHero` only after the pipeline compiled and the first frame was
  submitted (see `vgpu docs cat concepts-compilation.md`), so a failed compile
  keeps the poster instead of fading in an empty canvas.
