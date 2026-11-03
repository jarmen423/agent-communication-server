// The exposure's memory: drawn with blend (zero, src-alpha), so the feedback
// target is multiplied by `keep` = exp(-dt / tau) before new light is added.

struct Fade { keep: f32 }
@group(0) @binding(0) var<uniform> fade: Fade;

@fragment fn fs_main(@location(0) uv: vec2f) -> @location(0) vec4f {
  return vec4f(0.0, 0.0, 0.0, fade.keep);
}
