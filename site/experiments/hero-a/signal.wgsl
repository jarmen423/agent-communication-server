// Signal box: lit routes as rows of panel lamps, message tiles, reply pulses and
// the hub-server lamp. Everything is in scene units (the SVG viewBox); the SVG
// under the canvas keeps the idle rails, cards, the lens and every label.

struct Board {
  view: vec4f,              // scene width, height, device px per unit, units per device px
  dims: vec4f,              // lamp width, halo sigma, lamp pitch, lamp gap
  sprite: vec4f,            // tile width, tile height, tile radius, pulse radius
  plate: vec4f,             // hub cx, cy, plate half size, plate radius
  lens: vec4f,              // units per mark unit, card radius, 0, 0
  teal: vec4f,
  orange: vec4f,
  lane: vec4f,
  cards: array<vec4f, 4>,   // x, y, w, h
  tracks: array<vec4f, 16>, // [2k] first segment, segment count, length, received; [2k+1] bbox min, max
  segs: array<vec4f, 128>,  // [2i] a.xy, b.xy; [2i+1] arc length at a, segment length
}

struct Live {
  lit: array<vec4f, 8>,      // lo, hi (arc length); lo >= hi is dark
  sprites: array<vec4f, 16>, // [2j] x, y, kind (0 none, 1 tile, 2 pulse), received; [2j+1] track, s
  hub: vec4f,                // sent lamp, received lamp
}

@group(0) @binding(0) var<uniform> board: Board;
@group(0) @binding(1) var<uniform> live: Live;

fn sat(x: f32) -> f32 { return clamp(x, 0.0, 1.0); }

/** Coverage of a signed distance (negative inside), anti-aliased over one device pixel. */
fn cover(d: f32) -> f32 { return sat(0.5 - d * board.view.z); }

fn sd_round_rect(p: vec2f, c: vec2f, half: vec2f, r: f32) -> f32 {
  let q = abs(p - c) - half + vec2f(r);
  return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

/** The brand's message tile: one sharp corner, bottom-left when sent, bottom-right when received. */
fn sd_tile(p: vec2f, c: vec2f, half: vec2f, r: f32, received: bool) -> f32 {
  let o = p - c;
  var rr = r;
  if (o.y > 0.0 && ((received && o.x > 0.0) || (!received && o.x < 0.0))) { rr = 0.0; }
  let q = abs(o) - half + vec2f(rr);
  return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

/** One rounded corner of an unbounded box: the quadrant `q` points into is rounded by r. */
fn sd_corner(q: vec2f, r: f32) -> f32 {
  return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

/** The lens of the mark (where the two bubbles overlap), so the hub lamp never paints over it. */
fn sd_lens(p: vec2f) -> f32 {
  let c = board.plate.xy;
  let u = board.lens.x;
  let teal = sd_corner(p - (c - vec2f(4.0 * u)), 10.0 * u);
  let orange = sd_corner((c + vec2f(4.0 * u)) - p, 10.0 * u);
  return max(teal, orange);
}

/** Distance to a track and the arc length of the nearest point on it. */
fn nearest(p: vec2f, first: u32, count: u32) -> vec2f {
  var best = vec2f(1e9, 0.0);
  for (var i = 0u; i < count; i++) {
    let g = board.segs[(first + i) * 2u];
    let h = board.segs[(first + i) * 2u + 1u];
    let ab = g.zw - g.xy;
    let t = sat(dot(p - g.xy, ab) / max(dot(ab, ab), 1e-6));
    let d = length(p - g.xy - ab * t);
    if (d < best.x) { best = vec2f(d, h.x + t * h.y); }
  }
  return best;
}

fn over(dst: vec4f, rgb: vec3f, a: f32) -> vec4f {
  return vec4f(rgb * a + dst.rgb * (1.0 - a), a + dst.a * (1.0 - a));
}

@fragment fn fs_main(@location(0) uv: vec2f) -> @location(0) vec4f {
  let p = uv * board.view.xy;
  let lampW = board.dims.x;
  let sigma = board.dims.y;
  let pitch = board.dims.z;
  let gap = board.dims.w;
  let tileW = board.sprite.x;
  var out = vec4f(0.0);

  // Lit tracks: a row of lamps, one per track section, each with a soft lamp halo.
  for (var k = 0u; k < 8u; k++) {
    let lit = live.lit[k];
    if (lit.y <= lit.x) { continue; }
    let box = board.tracks[k * 2u + 1u];
    if (any(p < box.xy) || any(p > box.zw)) { continue; }
    let info = board.tracks[k * 2u];
    let ns = nearest(p, u32(info.x), u32(info.y));
    let d = ns.x;
    let s = ns.y;
    let s0 = floor(s / pitch) * pitch;
    // A section lights as the route's head passes it and goes dark as the tail does.
    let ramp = pitch * 0.75;
    let on = sat((lit.y - s0) / ramp) * sat((s0 + pitch - lit.x) / ramp);
    if (on <= 0.0) { continue; }

    var occ = 0.0;
    var trail = 0.0;
    for (var j = 0u; j < 8u; j++) {
      let a = live.sprites[j * 2u];
      let b = live.sprites[j * 2u + 1u];
      if (a.z < 0.5 || u32(b.x + 0.5) != k) { continue; }
      let ds = s - b.y;
      if (a.z < 1.5) {
        occ = max(occ, exp(-(ds * ds) / (tileW * tileW)));
      } else {
        // Replies run toward s = 0, so the streak they leave is at larger s.
        let behind = select(-ds, ds, a.w > 0.5);
        if (behind > 0.0) { trail = max(trail, exp(-behind / (tileW * 0.9))); }
      }
    }

    let hw = lampW * 0.5;
    let lamp = cover(sd_corner(abs(vec2f(s - s0 - pitch * 0.5, d)) - vec2f((pitch - gap) * 0.5, hw) + vec2f(hw), hw));
    let halo = exp(-(d * d) / (sigma * sigma)) * (0.15 + 0.2 * occ + 0.22 * trail);
    let colour = select(board.teal.rgb, board.orange.rgb, info.w > 0.5);
    let hot = mix(colour, vec3f(1.0), 0.16 * max(occ, trail) * lamp);
    let a = on * (lamp * (0.82 + 0.18 * max(occ, trail)) + halo * (1.0 - lamp));
    out = over(out, hot, a);
  }

  // Reply pulses and message tiles.
  for (var j = 0u; j < 8u; j++) {
    let a = live.sprites[j * 2u];
    if (a.z < 0.5) { continue; }
    let colour = select(board.teal.rgb, board.orange.rgb, a.w > 0.5);
    var sd = 0.0;
    if (a.z < 1.5) {
      sd = sd_tile(p, a.xy, board.sprite.xy * 0.5, board.sprite.z, a.w > 0.5);
    } else {
      sd = length(p - a.xy) - board.sprite.w;
    }
    let g = max(sd, 0.0);
    let glow = exp(-(g * g) / (sigma * sigma * 1.6)) * 0.24;
    let body = cover(sd);
    if (a.z > 1.5) {
      // A pulse is a lamp set into a lit track of its own colour: give it a bezel.
      out = over(out, board.lane.rgb, cover(sd - lampW * 0.6));
    }
    out = over(out, colour, body + glow * (1.0 - body));
  }

  // Nothing draws over the node cards or into the hub plate.
  var mask = 1.0;
  for (var i = 0u; i < 4u; i++) {
    let c = board.cards[i];
    let half = c.zw * 0.5;
    mask = min(mask, 1.0 - cover(sd_round_rect(p, c.xy + half, half, board.lens.y)));
  }
  let plate = sd_round_rect(p, board.plate.xy, vec2f(board.plate.z), board.plate.w);
  mask = min(mask, 1.0 - cover(plate));
  out = out * mask;

  // The hub-server lamp: the plate glows behind the lens while an envelope passes through.
  let lampAmt = max(live.hub.x, live.hub.y);
  if (lampAmt > 0.002) {
    let colour = (board.teal.rgb * live.hub.x + board.orange.rgb * live.hub.y) / (live.hub.x + live.hub.y);
    let r = length(p - board.plate.xy) / board.plate.z;
    let inside = cover(plate + 1.5) * sat(0.5 + sd_lens(p) * board.view.z);
    let wash = lampAmt * 0.34 * exp(-r * r * 2.8) * inside;
    let rim = lampAmt * 0.32 * exp(-(plate * plate) / 0.8);
    out = over(out, colour, max(wash, rim));
  }
  return out;
}
