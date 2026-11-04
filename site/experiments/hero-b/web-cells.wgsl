// The ASCII ground's light, one texel per character cell, computed
// analytically: faint rings out from the hub (one per envelope routed) and
// from visitor clicks, both neutral. Around the copy block the ground yields:
// hub rings fall to a floor inside a feathered rounded rect; click rings only
// dim right behind the block (their own floor, a short feather). The tracks
// themselves are plain B's long exposure (web-trails.wgsl), drawn on top.
// Mirrors cellLight() in web-light.ts.
// Output: R neutral light, G yield (0 under the copy; for tests and tools).

struct Web {
  hub: vec2f,                 // hub centre, design units
  cell: vec2f,                // cell size, design units
  look: vec4f,                // ripple weight, click weight, click floor, click feather
  ripples: array<vec4f, 8>,   // radius, amp, width
  clicks: array<vec4f, 4>,    // x, y, radius, amp
  mask: vec4f,                // copy block x0, y0, x1, y1
  mask2: vec4f,               // feather, floor, corner radius
}

@group(0) @binding(0) var<uniform> web: Web;

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let u = (floor(p.xy) + 0.5) * web.cell;
  let d = length(u - web.hub);
  var ripple = 0.0;
  for (var r = 0; r < 8; r++) {
    let q = web.ripples[r];
    let x = (d - q.x) / q.z;
    ripple += q.y * exp(-x * x);
  }
  var click = 0.0;
  for (var c = 0; c < 4; c++) {
    let q = web.clicks[c];
    let x = (length(u - q.xy) - q.z) / (26.0 + q.z * 0.05);
    click += q.w * exp(-x * x);
  }
  let half = (web.mask.zw - web.mask.xy) * 0.5;
  let r = web.mask2.z;
  let m = abs(u - (web.mask.xy + half)) - half + vec2f(r);
  let sd = length(max(m, vec2f(0.0))) + min(max(m.x, m.y), 0.0) - r;
  let yielded = smoothstep(0.0, web.mask2.x, sd);
  let k = web.mask2.y + (1.0 - web.mask2.y) * yielded;
  let kc = web.look.z + (1.0 - web.look.z) * smoothstep(0.0, web.look.w, sd);
  return vec4f(ripple * web.look.x * k + click * web.look.y * kc, yielded, 0.0, 1.0);
}
