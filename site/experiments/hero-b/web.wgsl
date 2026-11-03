// The ASCII web, drawn: a still grid of protocol text across the whole hero
// section (cover-mapped from the design space), each cell lit from its
// web-cells texel: a quiet lane-muted texture, lifted toward ink along live
// lines and ripples, and taking teal (sent) or orange (received) under a
// pulse, where spaces step up a density ramp. Under the glass hub plate, the
// lamp: a neutral (ink) glow that follows throughput and a rim flash in each
// envelope's own colour. Mirrors cellLook()/rampAt() in web-light.ts.

struct Field {
  grid: vec2f,       // columns, rows
  cell: vec2f,       // cell size, design units
  slot: vec2f,       // atlas slot size, texels
  atlas_cols: f32,
  unused: f32,
  map: vec4f,        // device px per design unit, offset x, offset y (device px)
  muted: vec4f,
  ink: vec4f,
  teal: vec4f,
  orange: vec4f,
  look: vec4f,       // base alpha, lift alpha, gain, lit scale
  look2: vec4f,      // hue speed, ink mix
  ramp_at: vec4f,
  ramp: vec4f,       // glyph indices of the ramp steps
}

struct Lamp {
  plate: vec4f,      // x, y, w, h (design units)
  glow: vec4f,       // glow 0..1, corner radius, ceiling
  events: array<vec4f, 12>,  // x, y, age (s), weight (+ sent, - received)
}

@group(0) @binding(0) var cells: texture_2d<f32>;
@group(0) @binding(1) var text: texture_2d<u32>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> field: Field;
@group(0) @binding(5) var<uniform> lamp: Lamp;

fn glyph(u: vec2f) -> vec4f {
  let cf = u / field.cell;
  let cell = clamp(vec2i(floor(cf)), vec2i(0), vec2i(field.grid) - vec2i(1));
  let light = textureLoad(cells, cell, 0).rgb;
  let es = 1.0 - exp(-field.look.z * light.r);
  let er = 1.0 - exp(-field.look.z * light.g);
  let a = 1.0 - (1.0 - es) * (1.0 - er);
  let n = 1.0 - exp(-light.b);
  let quiet = field.look.x + field.look.y * n;
  let alpha = quiet + (1.0 - quiet) * min(1.0, a * field.look.w);
  var g = textureLoad(text, cell, 0).r;
  if (g == 0u) {
    for (var i = 0; i < 4; i++) {
      if (a >= field.ramp_at[i]) { g = u32(field.ramp[i]); }
    }
  }
  let cols = u32(field.atlas_cols);
  let slot = vec2f(f32(g % cols), f32(g / cols));
  let uv = (slot * field.slot + fract(cf) * field.cell * field.map.x) / vec2f(textureDimensions(atlas));
  let cov = textureSampleLevel(atlas, samp, uv, 0.0).r;
  let quiet_rgb = mix(field.muted.rgb, field.ink.rgb, n * field.look2.y);
  let hue = mix(field.teal.rgb, field.orange.rgb, er / max(es + er, 1e-5));
  let rgb = mix(quiet_rgb, hue, min(1.0, a * field.look2.x));
  return vec4f(rgb * alpha * cov, alpha * cov);
}

fn lamp_light(u: vec2f) -> vec4f {
  let half = lamp.plate.zw * 0.5;
  let r = lamp.glow.y;
  let q = abs(u - (lamp.plate.xy + half)) - half + vec2f(r);
  let sd = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
  if (sd > 140.0) { return vec4f(0.0); }
  let glow = lamp.glow.x * exp(-max(sd, 0.0) / 34.0) * 0.5;
  var sent = 0.0;
  var received = 0.0;
  let ring = exp(-sd * sd / 10.0);
  for (var i = 0; i < 12; i++) {
    let ev = lamp.events[i];
    let env = (1.0 - exp(-ev.z / 0.07)) * exp(-ev.z / 0.45);
    let d = u - ev.xy;
    let f = abs(ev.w) * env * exp(-dot(d, d) / 600.0) * (0.25 + 0.75 * ring);
    if (ev.w > 0.0) { sent += f; } else { received += f; }
  }
  let es = 1.0 - exp(-1.3 * sent);
  let er = 1.0 - exp(-1.3 * received);
  let flash = 1.0 - (1.0 - es) * (1.0 - er);
  let hue = mix(field.teal.rgb, field.orange.rgb, er / max(es + er, 1e-5));
  // Neutral lamp under the coloured flashes: ink glow, never a teal/orange mix.
  let a = 1.0 - (1.0 - glow) * (1.0 - flash);
  let rgb = (field.ink.rgb * glow * (1.0 - flash) + hue * flash) * lamp.glow.z;
  return vec4f(rgb, a * lamp.glow.z);
}

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let u = (p.xy - field.map.yz) / field.map.x;
  let g = glyph(u);
  return g + lamp_light(u) * (1.0 - g.a);
}
