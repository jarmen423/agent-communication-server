// The ASCII web's light, one texel per character cell, computed analytically
// (no feedback buffer): for every live line, the distance from the cell to
// the line and the arc length there; the line lifts nearby cells while alive,
// and each signal is a pulse with a sharp front and a fading tail. Ripples
// from the hub add rings. Mirrors cellLight() in web-light.ts, which renders
// the server poster. Output: R sent (teal), G received (orange), B neutral.

struct Web {
  grid: vec2f,                // columns, rows
  cell: vec2f,                // cell size, design units
  hub: vec2f,                 // hub centre, design units
  sigma: f32,                 // line proximity width (about one row)
  lines: f32,                 // live line count
  look: vec4f,                // pulse front width, lift weight, ripple weight, unused
  line_info: array<vec4f, 6>, // per line: alive, pulse count
  pts: array<vec4f, 48>,      // per line 16 points, two per vec4
  pulses: array<vec4f, 36>,   // per line 6: head arc length, amp (+ sent, - received), direction, tail
  ripples: array<vec4f, 8>,   // radius, amp, width
}

@group(0) @binding(0) var<uniform> web: Web;

fn point(i: i32, j: i32) -> vec2f {
  let v = web.pts[i * 8 + j / 2];
  return select(v.xy, v.zw, (j % 2) == 1);
}

@fragment fn fs_main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  let u = (floor(p.xy) + 0.5) * web.cell;
  var sent = 0.0;
  var received = 0.0;
  var lift = 0.0;
  let n = i32(web.lines);
  for (var i = 0; i < 6; i++) {
    if (i >= n) { break; }
    var best = 1e20;
    var best_s = 0.0;
    var run = 0.0;
    for (var j = 0; j < 15; j++) {
      let a = point(i, j);
      let b = point(i, j + 1) - a;
      let len2 = max(dot(b, b), 1e-6);
      let t = clamp(dot(u - a, b) / len2, 0.0, 1.0);
      let d = u - a - b * t;
      let len = sqrt(len2);
      if (dot(d, d) < best) {
        best = dot(d, d);
        best_s = run + t * len;
      }
      run += len;
    }
    let prox = exp(-best / (2.0 * web.sigma * web.sigma));
    lift += web.line_info[i].x * prox;
    let m = i32(web.line_info[i].y);
    for (var k = 0; k < 6; k++) {
      if (k >= m) { break; }
      let q = web.pulses[i * 6 + k];
      let ds = (best_s - q.x) * q.z;
      var shape = exp(ds / q.w);
      if (ds > 0.0) { shape = exp(-ds * ds / (2.0 * web.look.x * web.look.x)); }
      let e = abs(q.y) * prox * shape;
      if (q.y > 0.0) { sent += e; } else { received += e; }
    }
  }
  let d = length(u - web.hub);
  var ripple = 0.0;
  for (var r = 0; r < 8; r++) {
    let q = web.ripples[r];
    let x = (d - q.x) / q.z;
    ripple += q.y * exp(-x * x);
  }
  return vec4f(sent, received, lift * web.look.y + ripple * web.look.z, 1.0);
}
