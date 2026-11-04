// The web's message tracks, developed: plain B's long-exposure light (R sent,
// G received) soft-saturated into teal and orange, premultiplied, drawn over
// the ASCII ground. On the dark ground orange reads much brighter than teal,
// so sent light gets a little more gain: both directions read equally. The
// light target may be smaller than the output (soft
// light survives the upscale), so it is sampled. Around the copy block the
// tracks yield (a feathered rounded rect, as in web-cells.wgsl), less behind
// the headline than behind the paragraph (head: the reaches show there).

struct Trails {
  teal: vec4f,
  orange: vec4f,
  size: vec2f,       // output size, pixels
  gain: vec2f,       // soft-saturation gain: sent, received
  map: vec4f,        // output px per design unit, offset x, offset y
  mask: vec4f,       // copy block x0, y0, x1, y1 (design units)
  mask2: vec4f,      // feather, floor, corner radius
  head: vec4f,       // headline floor, headline bottom y (design units), ramp
}

@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> trails: Trails;

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let v = textureSampleLevel(light, samp, p.xy / trails.size, 0.0);
  let sent = 1.0 - exp(-trails.gain.x * max(v.r, 0.0));
  let received = 1.0 - exp(-trails.gain.y * max(v.g, 0.0));
  let a = 1.0 - (1.0 - sent) * (1.0 - received);
  let share = received / max(sent + received, 1e-5);
  let u = (p.xy - trails.map.yz) / trails.map.x;
  let half = (trails.mask.zw - trails.mask.xy) * 0.5;
  let r = trails.mask2.z;
  let m = abs(u - (trails.mask.xy + half)) - half + vec2f(r);
  let sd = length(max(m, vec2f(0.0))) + min(max(m.x, m.y), 0.0) - r;
  let low = mix(trails.head.x, trails.mask2.y, smoothstep(trails.head.y, trails.head.y + trails.head.z, u.y));
  let k = low + (1.0 - low) * smoothstep(0.0, trails.mask2.x, sd);
  return vec4f(mix(trails.teal.rgb, trails.orange.rgb, share) * a, a) * k;
}
