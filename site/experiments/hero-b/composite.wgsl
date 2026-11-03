// Develop the exposure: soft-saturate each channel's light and map it to the
// brand colours (sent = teal, received = orange), premultiplied over the lane
// band so its dot texture shows through. Where the two overlap the hue is a
// share-weighted mix, never a sum, so crossings never blow out to white.
//
// Under the trails, the hub lamp (light shows through the glass plate, the
// lens stays opaque ink in SVG): a neutral ink glow that follows throughput,
// a rim flash in each envelope's own colour where it enters, and a faint ink
// ring rippling out for every envelope routed through the hub.

struct Look {
  teal: vec4f,
  orange: vec4f,
  gain: f32,
  opacity: f32,
}

struct Lamp {
  plate: vec4f,              // x, y, w, h (viewBox units)
  glow: vec4f,               // glow 0..1, corner radius, ceiling, device px per unit
  ink: vec4f,
  events: array<vec4f, 12>,  // x, y, age (s), weight (+ sent, - received)
  ripples: array<vec4f, 6>,  // radius, amp, width
}

@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var<uniform> look: Look;
@group(0) @binding(2) var<uniform> lamp: Lamp;

fn lamp_light(u: vec2f) -> vec4f {
  let half = lamp.plate.zw * 0.5;
  let c = lamp.plate.xy + half;
  let r = lamp.glow.y;
  let q = abs(u - c) - half + vec2f(r);
  let sd = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
  var ripple = 0.0;
  let d = length(u - c) - half.x;
  for (var i = 0; i < 6; i++) {
    let k = lamp.ripples[i];
    let x = (d - k.x) / k.z;
    ripple += k.y * exp(-x * x);
  }
  let glow = lamp.glow.x * exp(-max(sd, 0.0) / 20.0) * 0.55 + min(ripple, 1.0) * 0.04;
  var sent = 0.0;
  var received = 0.0;
  if (sd < 48.0) {
    let ring = exp(-sd * sd / 8.0);
    for (var i = 0; i < 12; i++) {
      let ev = lamp.events[i];
      let env = (1.0 - exp(-ev.z / 0.07)) * exp(-ev.z / 0.45);
      let e = u - ev.xy;
      let f = abs(ev.w) * env * exp(-dot(e, e) / 392.0) * (0.3 + 0.7 * ring);
      if (ev.w > 0.0) { sent += f; } else { received += f; }
    }
  }
  let es = 1.0 - exp(-1.2 * sent);
  let er = 1.0 - exp(-1.2 * received);
  let flash = 1.0 - (1.0 - es) * (1.0 - er);
  let hue = mix(look.teal.rgb, look.orange.rgb, er / max(es + er, 1e-5));
  let a = 1.0 - (1.0 - glow) * (1.0 - flash);
  return vec4f(lamp.ink.rgb * glow * (1.0 - flash) + hue * flash, a) * lamp.glow.z;
}

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let v = textureLoad(light, vec2i(p.xy), 0);
  let sent = 1.0 - exp(-look.gain * max(v.r, 0.0));
  let received = 1.0 - exp(-look.gain * max(v.g, 0.0));
  let a = 1.0 - (1.0 - sent) * (1.0 - received);
  let share = received / max(sent + received, 1e-5);
  let trails = vec4f(mix(look.teal.rgb, look.orange.rgb, share) * a, a) * look.opacity;
  return trails + lamp_light(p.xy / lamp.glow.w) * (1.0 - trails.a);
}
