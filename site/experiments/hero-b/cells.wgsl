// Gather: one texel per character cell holding the brightest light inside it
// (max, not mean, so a one-pixel trail still lights its cell), soft-saturated
// per channel: R = sent (teal), G = received (orange). The target is only
// columns × rows texels, so this extra pass is small.

struct Gather {
  cell: vec2f,   // cell size in light-texture pixels
  stride: f32,   // sample every n-th pixel (trails are at least ~2 px wide at n)
  gain: f32,
}

@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var<uniform> gather: Gather;

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let c = floor(p.xy);
  let dims = vec2i(textureDimensions(light));
  let lo = vec2i(c * gather.cell);
  let hi = min(vec2i((c + 1.0) * gather.cell), dims);
  let s = max(i32(gather.stride), 1);
  var m = vec2f(0.0);
  for (var y = lo.y; y < hi.y; y += s) {
    for (var x = lo.x; x < hi.x; x += s) {
      m = max(m, textureLoad(light, vec2i(x, y), 0).rg);
    }
  }
  return vec4f(1.0 - exp(-gather.gain * m), 0.0, 1.0);
}
