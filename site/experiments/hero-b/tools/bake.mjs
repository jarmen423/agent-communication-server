// Bake the poster frames from the real shaders: the same exposure the live
// layer draws as its first frame (schedule.POSTER_T), rendered headless with
// vgpu/node, then stored as an 8-bit indexed PNG: every pixel is the sent
// colour, the received colour or a mix of the two at some alpha, so a
// 256-entry palette (3 hues × 85 alphas + transparent) holds it almost exactly.
//
//   node site/experiments/hero-b/tools/bake.mjs
//
// The wide still is baked at 1x (soft light survives upscaling; 2x would be
// ~135 KB); the tall one at 2x, since phones are dense and it is small.
//
// Writes site/experiments/hero-b/posters/{wide,tall}.png, and for the ASCII
// variant posters/ascii-{wide,tall}.json: the gathered light per character
// cell (cells.wgsl), which the server renders as an SVG text still.
import { register } from "node:module";
import { mkdirSync, writeFileSync } from "node:fs";
import { encodeIndexed, encodePng } from "./png.mjs";

process.env.HERO_VGPU = "vgpu/node";
register("./hooks.mjs", import.meta.url);

const { init, target, frame } = await import("vgpu/node");
const { createExposure } = await import("../scene.ts");
const { wideLayout, tallLayout } = await import("../layout.ts");
const { createLight } = await import("../light.ts");
const { createCells } = await import("../ascii.ts");
const { GRID, TUNE } = await import("../field.ts");

const SCALE = { wide: 1, tall: 2 };
// Brand lane colours (identical in both themes): --lane-teal, --lane-orange.
const hex = (h) => [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
const palette = { teal: hex("3cb8ab"), orange: hex("f5a742") };
const out = new URL("../posters/", import.meta.url);
mkdirSync(out, { recursive: true });

const gpu = await init();
for (const [name, layout] of [["wide", wideLayout()], ["tall", tallLayout()]]) {
  const size = [Math.round(layout.width * SCALE[name]), Math.round(layout.height * SCALE[name])];
  const still = target(gpu, { size, format: "rgba8unorm" });
  const ex = createExposure(gpu, layout, palette, size, { lamp: false });
  await ex.compile(still);
  frame(gpu, (f) => ex.encode(f, still, 0));
  const px = await still.color.read({ mipLevel: 0, region: "all" });
  const png = encodeIndexed(size[0], size[1], px, palette);
  writeFileSync(new URL(`${name}.png`, out), png);
  // HERO_PREVIEW=<dir>: also write the still over the light-theme lane colour, for eyeballing.
  if (process.env.HERO_PREVIEW) {
    const lane = [0x17, 0x1a, 0x21];
    const flat = new Uint8Array(px.length);
    for (let i = 0; i < px.length; i += 4) {
      for (let c = 0; c < 3; c++) flat[i + c] = Math.min(255, px[i + c] + Math.round(lane[c] * (1 - px[i + 3] / 255)));
      flat[i + 3] = 255;
    }
    writeFileSync(`${process.env.HERO_PREVIEW}/${name}.png`, encodePng(size[0], size[1], flat));
  }
  console.log(`${name}: ${size.join("×")}, ${ex.count} deposits, ${(png.length / 1024).toFixed(1)} KB`);
  ex.dispose();
}
// ASCII variant: per-cell light at the poster moment, 4 bits per channel (sent, received).
for (const [name, layout] of [["wide", wideLayout(undefined, TUNE.wide)], ["tall", tallLayout(undefined, TUNE.tall)]]) {
  const size = [layout.width * 2, layout.height * 2];
  const grid = GRID[name];
  const ex = createLight(gpu, layout, size, { through: true });
  const cells = createCells(gpu, ex.light, grid, 2);
  await Promise.all([ex.compile(), cells.compile()]);
  frame(gpu, (f) => {
    ex.encode(f, 0);
    cells.encode(f);
  });
  const v = await cells.cells.color.readFloats({ mipLevel: 0, region: "all" });
  let hexes = "";
  for (let i = 0; i < grid[0] * grid[1]; i++) {
    const q = (x) => Math.max(0, Math.min(15, Math.round(x * 15)));
    hexes += q(v[i * 4]).toString(16) + q(v[i * 4 + 1]).toString(16);
  }
  writeFileSync(new URL(`ascii-${name}.json`, out), JSON.stringify({ grid, cells: hexes }));
  console.log(`ascii-${name}: ${grid.join("×")} cells, ${ex.count} deposits, ${(hexes.length / 1024).toFixed(1)} KB`);
  ex.dispose();
  cells.dispose();
}
gpu.dispose();
