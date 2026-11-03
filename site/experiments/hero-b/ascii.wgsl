// The ASCII field and the hub lamp, in one fullscreen pass.
//
// Field: a still grid of glyph cells. Each cell's text comes from the hub's
// own traffic (the `text` index texture); its light comes from the gathered
// exposure (`cells`). Unlit, the text is a dim lane-muted texture; lit, it
// brightens in teal (sent) or orange (received), and spaces step up a density
// ramp so a trail never breaks between words. Mirrors field.ts (cellLook,
// glyphAt, litAlpha), which draws the same rules into the SVG poster.
//
// Lamp: smooth light under the glass hub plate. Every envelope registers
// where it enters (a short rim flash in its own colour, soft attack, ~0.5 s),
// and a slow glow follows recent throughput. The lens stays opaque ink in SVG.

struct Field {
  grid: vec2f,       // columns, rows
  cell: vec2f,       // cell size in output pixels
  slot: vec2f,       // atlas slot size in texels
  atlas_cols: f32,
  px: f32,           // output pixels per viewBox unit
  muted: vec4f,
  teal: vec4f,
  orange: vec4f,
  look: vec4f,       // base alpha, lit scale, lit power, edge fade (cells)
  ramp_at: vec4f,    // light thresholds for the ramp steps
  ramp: vec4f,       // glyph indices of the ramp steps
}

struct Lamp {
  plate: vec4f,      // x, y, w, h (viewBox units)
  glow: vec4f,       // glow 0..1, received share, corner radius, ceiling
  events: array<vec4f, 12>,  // x, y, age (s), weight (+ sent, - received)
}

@group(0) @binding(0) var cells: texture_2d<f32>;
@group(0) @binding(1) var text: texture_2d<u32>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> field: Field;
@group(0) @binding(5) var<uniform> lamp: Lamp;

fn glyph_color(p: vec2f) -> vec4f {
  let cf = p / field.cell;
  let cell = clamp(vec2i(floor(cf)), vec2i(0), vec2i(field.grid) - vec2i(1));
  let local = fract(cf);
  let e = textureLoad(cells, cell, 0).rg;
  let a = 1.0 - (1.0 - e.x) * (1.0 - e.y);
  let share = e.y / max(e.x + e.y, 1e-5);
  var g = textureLoad(text, cell, 0).r;
  if (g == 0u) {
    for (var i = 0; i < 4; i++) {
      if (a >= field.ramp_at[i]) { g = u32(field.ramp[i]); }
    }
  }
  let cols = u32(field.atlas_cols);
  let slot = vec2f(f32(g % cols), f32(g / cols));
  let uv = (slot * field.slot + local * field.cell) / vec2f(textureDimensions(atlas));
  let cov = textureSampleLevel(atlas, samp, uv, 0.0).r;
  // The dim texture dissolves into the band at the edges; lit cells never fade.
  let c = vec2f(cell) + 0.5;
  let edge = min(min(c.x, field.grid.x - c.x) / field.look.w, min(c.y, field.grid.y - c.y) / (field.look.w * 0.67));
  let base = field.look.x * clamp(edge, 0.0, 1.0);
  let lit = clamp(pow(a * field.look.y, field.look.z), 0.0, 1.0);
  let alpha = (base + (1.0 - base) * lit) * cov;
  let hue = mix(field.teal.rgb, field.orange.rgb, share);
  let rgb = mix(field.muted.rgb, hue, clamp(a * 6.0, 0.0, 1.0));
  return vec4f(rgb * alpha, alpha);
}

fn lamp_color(p: vec2f) -> vec4f {
  let u = p / field.px;
  let half = lamp.plate.zw * 0.5;
  let r = lamp.glow.z;
  let q = abs(u - (lamp.plate.xy + half)) - half + vec2f(r);
  let sd = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
  let g = lamp.glow.x * exp(-max(sd, 0.0) / 22.0) * 0.55;
  var sent = g * (1.0 - lamp.glow.y);
  var received = g * lamp.glow.y;
  if (sd < 64.0) {
    let ring = exp(-sd * sd / 8.0);
    for (var i = 0; i < 12; i++) {
      let ev = lamp.events[i];
      let env = (1.0 - exp(-ev.z / 0.07)) * exp(-ev.z / 0.45);
      let d = u - ev.xy;
      let f = abs(ev.w) * env * exp(-dot(d, d) / 392.0) * (0.3 + 0.7 * ring);
      if (ev.w > 0.0) { sent += f; } else { received += f; }
    }
  }
  let es = 1.0 - exp(-1.2 * sent);
  let eo = 1.0 - exp(-1.2 * received);
  let a = (1.0 - (1.0 - es) * (1.0 - eo)) * lamp.glow.w;
  let hue = mix(field.teal.rgb, field.orange.rgb, eo / max(es + eo, 1e-5));
  return vec4f(hue * a, a);
}

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let glyph = glyph_color(p.xy);
  return glyph + lamp_color(p.xy) * (1.0 - glyph.a);
}
