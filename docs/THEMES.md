# Visualizer themes

Agent sprites are drawn procedurally from a **theme manifest** — no image
assets, no third-party artwork. The default theme
(`visualizer/themes/default/theme.json`) is original work for nats-hub and
redistributable under BUSL-1.1 like the rest of the visualizer.

## Manifest format

`visualizer/themes/<name>/theme.json`:

```json
{
  "name": "default",
  "version": 1,
  "cell": { "width": 64, "height": 70, "hitRadius": 27, "labelOffset": 41 },
  "statusAnimations": {
    "idle":    { "loopMs": 1100, "bobAmp": 2.5, "bobHz": 0.0016 },
    "run":     { "loopMs": 620,  "bobAmp": 4.5, "bobHz": 0.004 },
    "review":  { "loopMs": 900,  "bobAmp": 2,   "bobHz": 0.002, "tilt": 0.08 },
    "failed":  { "loopMs": 1300, "bobAmp": 1,   "bobHz": 0.001, "shake": 1.6 },
    "waiting": { "loopMs": 1400, "bobAmp": 1.5, "bobHz": 0.0012 }
  },
  "variants": [
    { "id": "orb-bot", "label": "Orb Bot", "shape": "orb",
      "body": "#2dd4bf", "accent": "#99f6e4", "eye": "#06281f" }
  ]
}
```

- `variants[].shape` is one of `orb`, `hex`, `cat`, `ghost` (canvas
  primitives drawn by `visualizer/js/theme.js`). `body`/`accent`/`eye` are
  CSS colors.
- `statusAnimations` maps agent status to motion: `working → run`,
  `thinking → review`, `error → failed`, otherwise `idle`; stopped agents
  render muted.

## Identity → variant assignment

An identity is assigned `variants[hash(identity) % variants.length]`
(stable per identity). The spawn wizard's variant picker, or a stored
instance's `petSlug` field in `localStorage nats-hub.instances`, overrides
it for that instance.

## Add a theme

1. Create `visualizer/themes/<name>/theme.json` as above with at least one
   variant.
2. Load it from `visualizer/js/theme.js` (`Theme.load('<name>')`), or make
   it the default by replacing `themes/default/theme.json`.
3. Restart `hub-server` (static files are served from `--static-dir`).

Verify: open the visualizer, start a worker (e.g. `make up`), and confirm
agents render with the new palette/shapes. The Playwright smoke test
(`tests/e2e_visualizer/run_smoke.sh`) screenshots the default theme to
`tests/e2e_visualizer/artifacts/default-theme.png`.
