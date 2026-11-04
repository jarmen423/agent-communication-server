// Generates app/tokens.css from ../brand/tokens.json (the brand's source of truth).
//
// Brand values are copied through untouched. The in-between steps the brand does
// not define (hover, pressed, tints, a stronger hairline, text on the `lane` band)
// are derived in OKLCH with the offsets bespoke's theme.py uses, so a brand change
// flows through without hand edits. Run: `npm run tokens` (also runs before dev/build).
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const brandPath = join(here, "..", "..", "brand", "tokens.json");
const outPath = join(here, "..", "app", "tokens.css");
const brand = JSON.parse(readFileSync(brandPath, "utf8"));

// ---- OKLCH helpers (same math as theme.py) ----
const toLin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const fromLin = (c) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);
function hexToRgb(hex) {
  const h = hex.replace("#", "");
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
}
function rgbToHex([r, g, b]) {
  const c = (v) => Math.round(Math.min(1, Math.max(0, v)) * 255).toString(16).padStart(2, "0");
  return `#${c(r)}${c(g)}${c(b)}`;
}
function toOklch(hex) {
  const [r, g, b] = hexToRgb(hex).map(toLin);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return [L, Math.hypot(A, B), ((Math.atan2(B, A) * 180) / Math.PI + 360) % 360];
}
function oklchToRgb(L, C, H) {
  const a = C * Math.cos((H * Math.PI) / 180);
  const b = C * Math.sin((H * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ].map(fromLin);
}
// Reduce chroma until the colour is inside sRGB, like theme.py's oklch_in_gamut.
function oklch(L, C, H) {
  let c = C;
  for (let i = 0; i < 40; i++) {
    const rgb = oklchToRgb(L, c, H);
    if (rgb.every((v) => v >= -1e-4 && v <= 1 + 1e-4)) return rgbToHex(rgb);
    c *= 0.94;
  }
  return rgbToHex(oklchToRgb(L, 0, H));
}
const shift = (hex, dL, cScale = 1) => {
  const [L, C, H] = toOklch(hex);
  return oklch(Math.min(0.99, Math.max(0.02, L + dL)), C * cScale, H);
};
const tint = (ground, colour, dL, cScale) => {
  const [gL] = toOklch(ground);
  const [, C, H] = toOklch(colour);
  return oklch(gL + dL, C * cScale, H);
};

// ---- brand colours per theme ----
const themes = brand.color.themes.map((t) => t.id);
const value = (name, theme) => {
  const token = brand.color.tokens.find((t) => t.name === name);
  if (!token) throw new Error(`tokens.json has no colour named ${name}`);
  return token.value[theme];
};

function derived(theme) {
  const dark = theme === "dark";
  const v = (n) => value(n, theme);
  // The lane band is dark in both themes, so text on it uses the dark theme's ink,
  // and teal/orange on it use the dark theme's brand values.
  const laneInk = value("ink", "dark");
  return {
    // theme.py: accent-hover = L+0.068, C x0.8 on dark. On light grounds hover darkens.
    "teal-hover": dark ? shift(v("relay-teal"), 0.068, 0.8) : shift(v("relay-teal"), -0.06),
    "teal-press": dark ? shift(v("relay-teal"), -0.04) : shift(v("relay-teal"), -0.1),
    "teal-tint": dark ? tint(v("paper"), v("relay-teal"), 0.05, 0.3) : tint(v("paper"), v("relay-teal"), -0.035, 0.22),
    "orange-hover": dark ? shift(v("signal-orange"), 0.05, 0.9) : shift(v("signal-orange"), -0.05),
    "orange-tint": dark ? tint(v("paper"), v("signal-orange"), 0.05, 0.25) : tint(v("paper"), v("signal-orange"), -0.03, 0.2),
    // theme.py: border-strong sits ~+0.063 L above border on dark; on light it darkens.
    "line-strong": dark ? shift(v("line"), 0.07) : shift(v("line"), -0.14),
    "surface-hover": dark ? shift(v("surface-raised"), 0.03) : shift(v("paper"), -0.015),
    ring: dark ? shift(v("relay-teal"), 0.068, 0.8) : v("relay-teal"),
    "lane-raised": shift(v("lane"), 0.035),
    "lane-line": shift(v("lane"), 0.085),
    "lane-ink": laneInk,
    "lane-muted": value("ink-muted", "dark"),
    "lane-teal": value("relay-teal", "dark"),
    "lane-orange": value("signal-orange", "dark"),
  };
}

function block(theme) {
  const lines = brand.color.tokens.map((t) => `  --${t.name}: ${t.value[theme]};`);
  for (const [k, val] of Object.entries(derived(theme))) lines.push(`  --${k}: ${val};`);
  return lines.join("\n");
}

// ---- type, spacing, radius ----
const fontVar = { display: "--font-bricolage", sans: "--font-plex-sans", mono: "--font-plex-mono" };
const families = Object.entries(brand.type.families).map(([id, stack]) => {
  // Swap the brand's first family for the next/font variable (self-hosted at build time),
  // keep the brand's fallbacks.
  const rest = stack.split(",").slice(1).join(",").trim();
  return `  --font-${id}: var(${fontVar[id]}), ${rest};`;
});
const px = (n) => (typeof n === "number" ? String(n) : n);
const textStyles = brand.type.groups.flatMap((g) =>
  g.styles.flatMap((s) => {
    const out = [`  --text-${s.name}: ${s.fontSize};`, `  --text-${s.name}--line-height: ${px(s.lineHeight)};`];
    if (s.letterSpacing) out.push(`  --text-${s.name}--letter-spacing: ${s.letterSpacing};`);
    out.push(`  --text-${s.name}--font-weight: ${s.fontWeight};`);
    return out;
  }),
);
const familyOf = Object.fromEntries(brand.type.groups.flatMap((g) => g.styles.map((s) => [s.name, g.family])));
const radii = brand.radius.tokens.map((t) => `  --radius-${t.name.replace("radius-", "")}: ${t.value};`);
const spaces = brand.spacing.tokens.map((t) => `  --${t.name}: ${t.value};`);
const colourNames = [...brand.color.tokens.map((t) => t.name), ...Object.keys(derived("light"))];

const css = `/* Generated by scripts/build-tokens.mjs from brand/tokens.json. Do not edit. */
:root {
  color-scheme: light;
${block("light")}
${spaces.join("\n")}
}
:root[data-theme="dark"],
[data-tone="dark"] {
  color-scheme: dark;
${block("dark")}
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
${block("dark").replace(/^/gm, "  ")}
  }
}
@theme inline {
${colourNames.map((n) => `  --color-${n}: var(--${n});`).join("\n")}
${families.join("\n")}
${textStyles.join("\n")}
${radii.join("\n")}
}
/* Brand type styles as utilities that also set the family: .type-display-xl … .type-mono */
${Object.entries(familyOf)
  .map(([name, fam]) => `@utility type-${name} {\n  font-family: var(--font-${fam});\n  font-size: var(--text-${name});\n  line-height: var(--text-${name}--line-height);\n  font-weight: var(--text-${name}--font-weight);\n  letter-spacing: var(--text-${name}--letter-spacing, normal);\n}`)
  .join("\n")}
`;

writeFileSync(outPath, css);
console.log(`tokens: wrote ${outPath.replace(join(here, ".."), "site")} (${themes.join(", ")}; ${colourNames.length} colours)`);

// ---- favicon: brand/logo/mark-light.svg, switching to mark-dark's fills on dark browser chrome ----
const markDir = join(here, "..", "..", "brand", "logo");
const fills = (svg) => [...svg.matchAll(/fill="(#[0-9a-fA-F]{6})"/g)].map((m) => m[1]);
const light = readFileSync(join(markDir, "mark-light.svg"), "utf8");
const darkFills = fills(readFileSync(join(markDir, "mark-dark.svg"), "utf8"));
const lightFills = fills(light);
if (lightFills.length !== 3 || darkFills.length !== 3) throw new Error("expected three fills in each brand mark");
let i = 0;
const classed = light
  .replace(/ width="\d+" height="\d+"/, "")
  .replace(/fill="#[0-9a-fA-F]{6}"/g, () => `class="f${i++}"`);
const style = `<style>${lightFills.map((c, n) => `.f${n}{fill:${c}}`).join("")}@media (prefers-color-scheme:dark){${darkFills
  .map((c, n) => `.f${n}{fill:${c}}`)
  .join("")}}</style>`;
writeFileSync(join(here, "..", "app", "icon.svg"), classed.replace(/(<svg[^>]*>)/, `$1${style}`));
console.log("tokens: wrote site/app/icon.svg from brand/logo/mark-{light,dark}.svg");
