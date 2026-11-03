// The paper and the pens: a quiet grid that scrolls with the tape, one trace per
// row drawn only up to the "now" playhead, the hub rail's two tracks, and soft
// fades at both ends. Output is premultiplied alpha over a transparent canvas,
// so the lane band shows through. Units are the layout's viewBox units.
struct Paper {
  view: vec4f,   // width, height, device px per unit, playhead x
  tape: vec4f,   // x0, x1, fade length, units per second
  axis: vec4f,   // axis y, bottom y, tape time (s), grid step (s)
  info: vec4f,   // row count, unused x3
  rows: array<vec4f, 8>, // y, 1 = hub rail
  ink: vec4f,
  line: vec4f,
  muted: vec4f,
}
@group(0) @binding(0) var<uniform> paper: Paper;

// Coverage of a line `d` units from its centre. Lines thinner than a device
// pixel are drawn one pixel wide at reduced alpha, so hairlines never shimmer.
fn line(d: f32, halfWidth: f32, px: f32) -> f32 {
  let w = max(halfWidth, 0.5 / px);
  return clamp((w - abs(d)) * px + 0.5, 0.0, 1.0) * (halfWidth / w);
}

fn over(dst: vec4f, rgb: vec3f, a: f32) -> vec4f {
  return vec4f(rgb * a, a) + dst * (1.0 - a);
}

fn within(v: f32, lo: f32, hi: f32) -> f32 {
  return step(lo, v) * step(v, hi);
}

@fragment fn fs_main(@location(0) uv: vec2f) -> @location(0) vec4f {
  let p = uv * paper.view.xy;
  let px = paper.view.z;
  let play = paper.view.w;
  let x0 = paper.tape.x;
  let x1 = paper.tape.y;
  let fadeLen = paper.tape.z;
  let pps = paper.tape.w;
  let axisY = paper.axis.x;
  let bottom = paper.axis.y;
  let now = paper.axis.z;
  let gridStep = paper.axis.w;

  let fade = smoothstep(x0, x0 + fadeLen, p.x) * (1.0 - smoothstep(x1 - fadeLen, x1, p.x));
  if (fade <= 0.0) {
    return vec4f(0.0);
  }
  var c = vec4f(0.0);

  // Tape time under this pixel; the grid is printed on the paper, so it moves.
  let t = now - (play - p.x) / pps;
  let dGrid = abs(fract(t / gridStep + 0.5) - 0.5) * gridStep * pps;
  let dSec = abs(fract(t + 0.5) - 0.5) * pps;
  c = over(c, paper.line.rgb, 0.7 * line(dGrid, 0.5, px) * within(p.y, axisY, bottom));
  c = over(c, paper.muted.rgb, 0.55 * line(dSec, 0.5, px) * within(p.y, axisY, axisY + 4.0));
  c = over(c, paper.muted.rgb, 0.8 * line(dGrid, 0.5, px) * within(p.y, axisY, axisY + 8.0));
  c = over(c, paper.muted.rgb, 0.4 * line(p.y - axisY, 0.5, px));

  // Traces exist only where a pen has already been: left of the playhead.
  let written = clamp((play - p.x) * px + 0.5, 0.0, 1.0);
  let count = i32(paper.info.x);
  for (var i = 0; i < count; i++) {
    let r = paper.rows[i];
    if (abs(p.y - r.x) > 6.0) {
      continue;
    }
    if (r.y > 0.5) {
      let rail = max(line(p.y - (r.x - 3.0), 0.5, px), line(p.y - (r.x + 3.0), 0.5, px));
      c = over(c, paper.ink.rgb, 0.3 * rail * written);
      let nib = min(length(p - vec2f(play, r.x - 3.0)), length(p - vec2f(play, r.x + 3.0))) - 1.8;
      c = over(c, paper.ink.rgb, 0.8 * clamp(0.5 - nib * px, 0.0, 1.0));
    } else {
      c = over(c, paper.line.rgb, line(p.y - r.x, 0.6, px) * written);
      let nib = length(p - vec2f(play, r.x)) - 2.4;
      c = over(c, paper.ink.rgb, 0.85 * clamp(0.5 - nib * px, 0.0, 1.0));
    }
  }

  // The playhead itself.
  c = over(c, paper.ink.rgb, 0.42 * line(p.x - play, 0.5, px) * within(p.y, axisY - 10.0, bottom + 6.0));

  return c * fade;
}
