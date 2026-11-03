struct Params { time: f32, teal: vec3f }
@group(0) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2f) -> @location(0) vec4f {
  let d = abs(uv.y - 0.5 - 0.1 * sin(uv.x * 12.0 + params.time * 2.0));
  let a = 1.0 - smoothstep(0.0, 0.02, d);
  return vec4f(params.teal * a, a);
}
