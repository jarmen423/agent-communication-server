// One light deposit per instance: a piece of a message's path, drawn as a
// flat-ended quad with a gaussian cross-section (a sharp core and a faint halo).
// Each deposit is weighted by its age, exp(-age / tau), so one draw can rebuild
// the whole exposure and per-frame draws stay frame-rate independent.
// Output: R = sent (teal) light, G = received (orange) light, added to the
// feedback target; the composite maps them to brand colours.

// Per-instance stream (a vertex buffer, not storage: some adapters, such as
// compatibility-mode devices, allow no storage buffers in the vertex stage).
//   ends: x0 y0 x1 y1, viewBox units
//   path: arc length s0 s1 (for dashes), age0 age1 (seconds)
//   look: channel (0 sent, 1 received), intensity, sigma, dash period (0 = solid)

struct View {
  size: vec2f,     // viewBox width, height
  px: f32,         // device pixels per viewBox unit
  tau: f32,        // seconds for light to fall to 1/e
  halo: f32,       // sigma of the soft halo, viewBox units
  halo_gain: f32,  // halo peak relative to the core
}

@group(0) @binding(0) var<uniform> view: View;

struct VOut {
  @builtin(position) pos: vec4f,
  @location(0) local: vec2f,
  @location(1) path: vec4f,  // constant per instance: no flat (compat devices reject its default sampling)
  @location(2) look: vec4f,
}

@vertex fn vs_main(
  @builtin(vertex_index) vi: u32,
  @location(0) ends: vec4f,
  @location(1) path: vec4f,
  @location(2) look: vec4f,
) -> VOut {
  var corners = array<vec2f, 6>(
    vec2f(0.0, -1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
    vec2f(0.0, -1.0), vec2f(1.0, 1.0), vec2f(0.0, 1.0),
  );
  let c = corners[vi];
  let sigma = max(look.z, 0.6 / view.px);
  let reach = 3.0 * max(sigma, view.halo);
  let d = ends.zw - ends.xy;
  let n = vec2f(-d.y, d.x) / max(length(d), 1e-4);
  let p = mix(ends.xy, ends.zw, c.x) + n * (c.y * reach);
  var out: VOut;
  out.pos = vec4f(p.x / view.size.x * 2.0 - 1.0, 1.0 - p.y / view.size.y * 2.0, 0.0, 1.0);
  out.local = vec2f(c.x, c.y * reach);
  out.path = path;
  out.look = look;
  return out;
}

@fragment fn fs_main(v: VOut) -> @location(0) vec4f {
  let sigma = v.look.z;
  let se = max(sigma, 0.6 / view.px);
  let d = abs(v.local.y);
  let age = mix(v.path.z, v.path.w, v.local.x);
  var dash = 1.0;
  if (v.look.w > 0.0) {
    // Progress ticks: the emitter blinks, like a beacon in a long exposure.
    let period = v.look.w;
    let ph = fract(mix(v.path.x, v.path.y, v.local.x) / period) * period;
    let edge = 1.0 / view.px;
    dash = clamp((period * 0.4 - ph) / edge + 0.5, 0.0, 1.0) * clamp(ph / edge + 0.5, 0.0, 1.0);
  }
  let core = exp(-0.5 * d * d / (se * se)) * (sigma / se);
  let halo = view.halo_gain * exp(-0.5 * d * d / (view.halo * view.halo));
  let weight = exp(-max(age, 0.0) / view.tau);
  let e = v.look.y * weight * (core + halo) * dash;
  let chan = select(vec2f(1.0, 0.0), vec2f(0.0, 1.0), v.look.x > 0.5);
  return vec4f(chan * e, 0.0, 0.0);
}
