// Develop the exposure: soft-saturate each channel's light and map it to the
// brand colours (sent = teal, received = orange), premultiplied over the lane
// band so its dot texture shows through. Where the two overlap the hue is a
// share-weighted mix, never a sum, so crossings never blow out to white.

struct Look {
  teal: vec4f,
  orange: vec4f,
  gain: f32,
  opacity: f32,
}

@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var<uniform> look: Look;

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let v = textureLoad(light, vec2i(p.xy), 0);
  let sent = 1.0 - exp(-look.gain * max(v.r, 0.0));
  let received = 1.0 - exp(-look.gain * max(v.g, 0.0));
  let a = 1.0 - (1.0 - sent) * (1.0 - received);
  let share = received / max(sent + received, 1e-5);
  let rgb = mix(look.teal.rgb, look.orange.rgb, share) * a;
  return vec4f(rgb, a) * look.opacity;
}
