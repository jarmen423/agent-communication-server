// The ink: every envelope on the tape as one instanced quad. Each mark is a
// rect (centre, half size) plus a style (kind, colour, alpha), written by the
// CPU each frame from the script. Kinds: 0 bar, 1 dot, 2 sent tile (sharp
// bottom-left), 3 received tile (sharp bottom-right), 4 pen glow.
struct Marks {
  view: vec4f,   // width, height, device px per unit, unused
  tape: vec4f,   // x0, x1, fade length, unused
  colors: array<vec4f, 4>, // teal, orange, ink, muted
  items: array<vec4f, 384>, // 2 per mark
}
@group(0) @binding(0) var<uniform> marks: Marks;

struct Out {
  @builtin(position) position: vec4f,
  @location(0) local: vec2f,
  @location(1) @interpolate(flat) half: vec2f,
  @location(2) @interpolate(flat) style: vec4f,
  @location(3) worldX: f32,
}

@vertex fn vs_main(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> Out {
  var corners = array<vec2f, 6>(vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0), vec2f(-1.0, 1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0));
  let rect = marks.items[i * 2u];
  let style = marks.items[i * 2u + 1u];
  let px = marks.view.z;
  // Pad by a device pixel and a half for antialiasing; a glow's quad is its radius.
  let pad = select(vec2f(1.5 / px), vec2f(0.0), style.x > 3.5);
  let ext = max(rect.zw, vec2f(0.5 / px)) + pad;
  let local = corners[v] * ext;
  let world = rect.xy + local;
  var out: Out;
  out.position = vec4f(world.x / marks.view.x * 2.0 - 1.0, 1.0 - world.y / marks.view.y * 2.0, 0.0, 1.0);
  out.local = local;
  out.half = rect.zw;
  out.style = style;
  out.worldX = world.x;
  return out;
}

// Rounded box with one radius per corner (y grows downward):
// r.x bottom-right, r.y top-right, r.z bottom-left, r.w top-left.
fn roundBox(p: vec2f, b: vec2f, r4: vec4f) -> f32 {
  var r = select(r4.zw, r4.xy, p.x > 0.0);
  let rr = select(r.y, r.x, p.y > 0.0);
  let q = abs(p) - b + vec2f(rr);
  return min(max(q.x, q.y), 0.0) + length(max(q, vec2f(0.0))) - rr;
}

@fragment fn fs_main(in: Out) -> @location(0) vec4f {
  let px = marks.view.z;
  let kind = in.style.x;
  var cover = 0.0;
  if (kind < 0.5) {
    // Bars thinner than a device pixel keep their ink by trading width for alpha.
    let b = max(in.half, vec2f(0.5 / px));
    let thin = (in.half.x / b.x) * (in.half.y / b.y);
    let d = roundBox(in.local, b, vec4f(min(b.x, b.y)));
    cover = clamp(0.5 - d * px, 0.0, 1.0) * thin;
  } else if (kind < 1.5) {
    cover = clamp(0.5 - (length(in.local) - in.half.x) * px, 0.0, 1.0);
  } else if (kind < 3.5) {
    let r = min(2.5, in.half.y);
    let radii = select(vec4f(r, r, 0.0, r), vec4f(0.0, r, r, r), kind > 2.5);
    cover = clamp(0.5 - roundBox(in.local, in.half, radii) * px, 0.0, 1.0);
  } else {
    let f = 1.0 - smoothstep(0.0, in.half.x, length(in.local));
    cover = f * f;
  }
  let x0 = marks.tape.x;
  let x1 = marks.tape.y;
  let fadeLen = marks.tape.z;
  let fade = smoothstep(x0, x0 + fadeLen, in.worldX) * (1.0 - smoothstep(x1 - fadeLen, x1, in.worldX));
  let a = cover * in.style.z * fade;
  let rgb = marks.colors[u32(in.style.y)].rgb;
  return vec4f(rgb * a, a);
}
