// Write the ASCII web's poster documents (see web-poster-svg.ts):
//   node site/experiments/hero-b/tools/bake-web.mjs
// → site/experiments/hero-b/posters/web.svg. Re-run after changing
// the web's model or look; scene.test.mjs fails if they drift.
import { register } from "node:module";
import { writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

register("./hooks.mjs", import.meta.url);
const { posterSvg } = await import("../web-poster-svg.ts");
const svg = posterSvg();
writeFileSync(new URL("../posters/web.svg", import.meta.url), svg);
console.log(`web.svg: ${(svg.length / 1024).toFixed(1)} KB, ${(gzipSync(svg, { level: 9 }).length / 1024).toFixed(1)} KB gzip`);
