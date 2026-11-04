// Write the ASCII web's posters:
//   node site/experiments/hero-b/tools/bake-web.mjs
// → posters/web.svg (the field, the hub glow and the place cards; see
//   web-poster-svg.ts; scene.test.mjs fails if it drifts), and
// → posters/web-trails-<band>.png: plain B's long exposure on the band's
//   tracks at the poster moment, from the real shaders (vgpu/node), the whole
//   design space (the page cover-maps it like the SVG, and a hero can be
//   wider than its band's viewport aspect), stored as an indexed PNG.
// HERO_PREVIEW=<dir> also writes each still over the dark paper, for eyeballing.
import { register } from "node:module";
import { writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { encodeIndexed, encodePng } from "./png.mjs";

process.env.HERO_VGPU = "vgpu/node";
register("./hooks.mjs", import.meta.url);
const { posterSvg } = await import("../web-poster-svg.ts");
const svg = posterSvg();
writeFileSync(new URL("../posters/web.svg", import.meta.url), svg);
console.log(`web.svg: ${(svg.length / 1024).toFixed(1)} KB, ${(gzipSync(svg, { level: 9 }).length / 1024).toFixed(1)} KB gzip`);

const { init, target, frame } = await import("vgpu/node");
const { WEB } = await import("../web-model.ts");
const { createTrails } = await import("../web-trails.ts");

/** Bake scale per band (px per design unit): soft light survives the upscale. */
const SCALE = { P: 0.75, M: 0.7, W: 0.7, tall: 1 };
// Brand lane colours (identical in both themes): --lane-teal, --lane-orange.
const hex = (h) => [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
const palette = { teal: hex("3cb8ab"), orange: hex("f5a742") };

const gpu = await init();
for (const [band, scale] of Object.entries(SCALE)) {
  const l = WEB[band];
  const size = [Math.round(l.width * scale), Math.round(l.height * scale)];
  const still = target(gpu, { size, format: "rgba8unorm" });
  const trails = createTrails(gpu, l, size, { cover: true, palette });
  await trails.compile(still);
  frame(gpu, (f) => {
    trails.ex.encode(f, 0);
    f.pass({ target: still, clear: [0, 0, 0, 0] }, (p) => p.draw(trails.develop));
  });
  const px = await still.color.read({ mipLevel: 0, region: "all" });
  const png = encodeIndexed(size[0], size[1], px, palette);
  writeFileSync(new URL(`../posters/web-trails-${band}.png`, import.meta.url), png);
  if (process.env.HERO_PREVIEW) {
    const paper = [0x10, 0x13, 0x17];
    const flat = new Uint8Array(px.length);
    for (let i = 0; i < px.length; i += 4) {
      for (let c = 0; c < 3; c++) flat[i + c] = Math.min(255, px[i + c] + Math.round(paper[c] * (1 - px[i + 3] / 255)));
      flat[i + 3] = 255;
    }
    writeFileSync(`${process.env.HERO_PREVIEW}/web-trails-${band}.png`, encodePng(size[0], size[1], flat));
  }
  console.log(`web-trails-${band}: ${size.join("×")}, ${trails.ex.count} deposits, ${(png.length / 1024).toFixed(1)} KB`);
  trails.dispose();
}
gpu.dispose();
