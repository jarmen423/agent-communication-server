// Concept B tests: the traffic follows the reply contract, the lanes keep the
// brand's direction rules, deposits stay sane, and startHero runs end to end
// on vgpu's mock adapter (no GPU, no browser).
//
//   node site/experiments/hero-b/scene.test.mjs
import { register } from "node:module";
import { test } from "node:test";
import assert from "node:assert/strict";

process.env.HERO_VGPU = "vgpu/mock";
register("./tools/hooks.mjs", import.meta.url);

const { wideLayout, tallLayout } = await import("./layout.ts");
const { createTraffic, Deposits, emit, exposure, HISTORY, POSTER_T, STRIDE } = await import("./schedule.ts");
const { WIDE, TALL } = await import("../../components/hero/geometry.ts");
const { startHero } = await import("./scene.ts");
const ascii = await import("./ascii.ts");
const { fieldRows, glyphTable, glyphAt, GRID, RAMP, TUNE } = await import("./field.ts");
const { hubLamp, LAMP_EVENTS } = await import("./schedule.ts");

const layouts = { wide: wideLayout(WIDE), tall: tallLayout(TALL) };

function collect(layout, until) {
  const traffic = createTraffic(layout);
  traffic.advance(until);
  return [...traffic.pending()];
}

for (const [name, layout] of Object.entries(layouts)) {
  test(`${name}: dozens of named agents on labelled machines`, () => {
    const ids = layout.agents.map((a) => a.id);
    assert.ok(ids.length >= 20 && ids.length <= 40, `${ids.length} agents`);
    assert.equal(new Set(ids).size, ids.length, "agent ids are unique");
    for (const m of ["laptop", "workstation", "build-box", "home-server", "ci-runner", "claude.ai"]) {
      assert.ok(layout.machines.some((x) => x.id === m), `machine ${m}`);
    }
    for (const id of ["me", "claude-1", "codex-1", "hermes-1", "claude-web-1"]) assert.ok(ids.includes(id), id);
    for (const a of layout.agents) {
      for (const l of [layout.lanes[a.id].toHub, layout.lanes[a.id].fromHub]) {
        assert.ok(l.length > 20);
        for (let i = 0; i < l.pts.length; i += 2) {
          assert.ok(l.pts[i] >= 0 && l.pts[i] <= layout.width && l.pts[i + 1] >= 0 && l.pts[i + 1] <= layout.height, `${a.id} lane inside the stage`);
        }
      }
    }
  });

  test(`${name}: teal (sent) and orange (received) keep their sides at the hub`, () => {
    // Wide: teal runs above orange. Tall: teal runs to the right of orange (left-hand traffic).
    const axis = layout.compact ? 0 : 1;
    const sign = layout.compact ? 1 : -1;
    for (const a of layout.agents) {
      const { toHub, fromHub } = layout.lanes[a.id];
      const atHub = { to: toHub.pts[toHub.pts.length - 2 + axis], from: fromHub.pts[axis] };
      const [teal, orange] = a.role === "orchestrator" ? [atHub.to, atHub.from] : [atHub.from, atHub.to];
      assert.ok(sign * (teal - orange) > 0, `${a.id}: sent lane on the expected side`);
    }
  });

  test(`${name}: every task follows the reply contract`, () => {
    const msgs = collect(layout, 240);
    const roles = new Map(layout.agents.map((a) => [a.id, a.role]));
    const tasks = Map.groupBy(msgs, (m) => m.task);
    assert.ok(tasks.size > 150, `${tasks.size} tasks in 4 minutes`);
    for (const [task, list] of tasks) {
      const sent = list.filter((m) => m.kind === "task");
      const results = list.filter((m) => m.kind === "result");
      const progress = list.filter((m) => m.kind === "status" || m.kind === "event");
      assert.equal(sent.length, 1, `${task}: one task out`);
      assert.equal(results.length, 1, `${task}: exactly one result`);
      assert.ok(progress.length >= 2, `${task}: progress ticks`);
      const [out] = sent;
      assert.equal(out.channel, 0, "a task is sent (teal)");
      assert.equal(roles.get(out.from), "orchestrator");
      assert.equal(roles.get(out.to), "worker");
      assert.ok(progress.some((m) => m.kind === "status" && m.detail === "working"));
      assert.ok(progress.some((m) => m.kind === "event" && m.detail === "started"));
      const delivered = out.t0 + out.total / out.speed;
      for (const m of [...progress, ...results]) {
        assert.equal(m.channel, 1, "replies are received (orange)");
        assert.equal(m.from, out.to);
        assert.equal(m.to, out.from);
        assert.ok(m.t0 > delivered, "replies start after the task arrives");
      }
      assert.ok(progress.every((m) => m.t0 < results[0].t0), "the result comes last");
    }
    assert.ok(tasks.has("task.7f3a2c1e") && tasks.get("task.7f3a2c1e")[0].to === "claude-1", "the hero log's task.7f3a");
  });

  test(`${name}: traffic is deterministic`, () => {
    const pick = (m) => [m.task, m.kind, m.from, m.to, m.t0, m.speed, m.drift];
    assert.deepEqual(collect(layout, 90).map(pick), collect(layout, 90).map(pick));
  });

  test(`${name}: the exposure fits its buffer and every deposit is in range`, () => {
    const traffic = createTraffic(layout);
    const out = exposure(traffic, layout, POSTER_T, new Deposits(32768));
    assert.ok(out.count > 1000 && out.count < 32768, `${out.count} deposits`);
    for (let i = 0; i < out.count; i++) {
      const d = out.data.subarray(i * STRIDE, (i + 1) * STRIDE);
      assert.ok(d[6] >= -1e-6 && d[7] >= -1e-6 && d[6] <= HISTORY + 1e-3, "ages are within the exposure");
      assert.ok(d[8] === 0 || d[8] === 1, "channel is sent or received");
      for (const [x, y] of [[d[0], d[1]], [d[2], d[3]]]) assert.ok(x > -8 && x < layout.width + 8 && y > -8 && y < layout.height + 8);
    }
  });

  test(`${name}: splitting a frame lays down the same light`, () => {
    const traffic = createTraffic(layout);
    const live = traffic.live(POSTER_T + 1);
    const length = (out) => {
      let s = 0;
      for (let i = 0; i < out.count; i++) s += out.data[i * STRIDE + 5] - out.data[i * STRIDE + 4];
      return s;
    };
    const whole = new Deposits(4096);
    const parts = new Deposits(4096);
    for (const m of live) {
      emit(m, POSTER_T, POSTER_T + 1, whole, layout);
      emit(m, POSTER_T, POSTER_T + 0.4, parts, layout);
      emit(m, POSTER_T + 0.4, POSTER_T + 1, parts, layout);
    }
    assert.ok(Math.abs(length(whole) - length(parts)) < 1e-2, "same arc length either way");
  });
}

// ---- startHero on the mock adapter ----

class MockCanvas extends EventTarget {
  width = 1200;
  height = 440;
  clientWidth = 1200;
  clientHeight = 440;
  getContext() {
    return { configure() {}, unconfigure() {}, getCurrentTexture: () => ({ createView: () => ({}) }) };
  }
}
globalThis.requestAnimationFrame ??= (cb) => setTimeout(() => cb(performance.now()), 4);
globalThis.cancelAnimationFrame ??= (id) => clearTimeout(id);

test("startHero renders, pauses, recolours and disposes on the mock adapter", async () => {
  const palette = { lane: [0, 0, 0], laneRaised: [0, 0, 0], laneLine: [0, 0, 0], laneInk: [1, 1, 1], laneMuted: [0.6, 0.6, 0.6], teal: [0.24, 0.72, 0.67], orange: [0.96, 0.65, 0.26] };
  for (const scene of [WIDE, TALL]) {
    const canvas = new MockCanvas();
    const hero = await startHero(canvas, { scene, palette });
    assert.equal(typeof hero.setPaused, "function");
    await new Promise((r) => setTimeout(r, 60));
    hero.setPaused(true);
    hero.setPalette({ ...palette, teal: [0, 1, 1] });
    canvas.dispatchEvent(new CustomEvent("hero:pause", { detail: true }));
    hero.setPaused(false); // still held by the visitor's pause
    canvas.dispatchEvent(new CustomEvent("hero:pause", { detail: false }));
    await new Promise((r) => setTimeout(r, 30));
    hero.dispose();
  }
});

// ---- the ASCII variant: light through the junction, the hub lamp, the field ----

const asciiLayouts = { wide: wideLayout(WIDE, TUNE.wide), tall: tallLayout(TALL, TUNE.tall) };

for (const [name, layout] of Object.entries(asciiLayouts)) {
  test(`${name}: light crosses the junction without a gap`, () => {
    const traffic = createTraffic(layout, 7, { through: true });
    traffic.advance(60);
    const { plate } = layout;
    for (const m of traffic.pending().slice(0, 80)) {
      assert.ok(m.mid, "a crossing path");
      const [ax, ay] = [m.a.pts.at(-2), m.a.pts.at(-1)];
      const [bx, by] = [m.b.pts[0], m.b.pts[1]];
      assert.ok(Math.hypot(m.mid.pts[0] - ax, m.mid.pts[1] - ay) < 1e-3, "starts where the lane enters");
      assert.ok(Math.hypot(m.mid.pts.at(-2) - bx, m.mid.pts.at(-1) - by) < 1e-3, "ends where the next lane leaves");
      const mx = m.mid.pts[16], my = m.mid.pts[17];
      assert.ok(mx > plate.x && mx < plate.x + plate.w && my > plate.y && my < plate.y + plate.h, "passes through the plate");
    }
    assert.equal(createTraffic(layout).live(POSTER_T)[0].mid, null, "light B keeps its hidden hold");
  });

  test(`${name}: the hub lamp registers each envelope in its own colour`, () => {
    const traffic = createTraffic(layout, 7, { through: true });
    const msgs = traffic.live(POSTER_T);
    const lamp = hubLamp(msgs, POSTER_T);
    assert.equal(lamp.events.length, LAMP_EVENTS);
    assert.ok(lamp.glow > 0.2 && lamp.glow < 1, `glow ${lamp.glow}`);
    assert.ok(lamp.share >= 0 && lamp.share <= 1);
    const live = lamp.events.filter((e) => e[2] < 99);
    assert.ok(live.length > 0, "something passed through recently");
    for (let i = 1; i < live.length; i++) assert.ok(live[i][2] >= live[i - 1][2], "freshest first");
    for (const [x, y] of live) {
      const p = layout.plate;
      assert.ok(Math.abs(x - p.x) < 12 || Math.abs(x - (p.x + p.w)) < 12 || Math.abs(y - p.y) < 12 || Math.abs(y - (p.y + p.h)) < 12, "flash sits on the plate's rim");
    }
    // Sent envelopes flash positive (teal), received negative (orange).
    const sentAt = new Set(msgs.filter((m) => m.channel === 0).map((m) => `${m.a.pts.at(-2)},${m.a.pts.at(-1)}`));
    for (const [x, y, , w] of live) if (w > 0) assert.ok(sentAt.has(`${x},${y}`));
  });

  test(`${name}: the field is the hub's own traffic, still and deterministic`, () => {
    const [cols, rows] = GRID[name];
    const text = fieldRows(layout, cols, rows);
    assert.equal(text.length, rows);
    for (const row of text) assert.equal(row.length, cols);
    assert.deepEqual(fieldRows(layout, cols, rows), text, "same text every time");
    const all = text.join("\n");
    for (const needle of ["channel.task.", "hub.pub.", '"status":"working"', '"event_type"']) assert.ok(all.includes(needle), needle);
    const { chars, index } = glyphTable(text);
    assert.equal(chars[0], " ");
    for (const c of RAMP.chars) assert.ok(chars.includes(c));
    assert.ok(Math.max(...index) < chars.length && chars.length < 256);
    assert.equal(glyphAt("x", 0.9), "x", "text stays text");
    assert.equal(glyphAt(" ", 0), " ");
    assert.equal(glyphAt(" ", 0.9), RAMP.chars.at(-1), "a bright space steps up the ramp");
  });
}

test("wide field rows carry the agents whose trunks pass through them", () => {
  const layout = asciiLayouts.wide;
  const [cols, rows] = GRID.wide;
  const text = fieldRows(layout, cols, rows);
  const row = Math.floor(102 / (layout.height / rows)); // the claude.ai gate's row
  assert.match(text.slice(row - 1, row + 2).join(" "), /web-1|web-2|claude-1|claude-3|claude-4|cursor-1|grok-1/);
});

test("the ASCII variant starts, recolours, resizes and disposes on the mock adapter", async () => {
  const fakeCtx = { font: "", fillStyle: "", textAlign: "", textBaseline: "", fillText() {}, getImageData: (x, y, w, h) => ({ data: new Uint8ClampedArray(w * h * 4) }) };
  globalThis.OffscreenCanvas ??= class { constructor(w, h) { this.width = w; this.height = h; } getContext() { return fakeCtx; } };
  globalThis.document ??= { fonts: { load: async () => [], ready: Promise.resolve() } };
  globalThis.getComputedStyle ??= () => ({ getPropertyValue: () => "'IBM Plex Mono', monospace" });
  const palette = { lane: [0, 0, 0], laneRaised: [0, 0, 0], laneLine: [0, 0, 0], laneInk: [1, 1, 1], laneMuted: [0.6, 0.6, 0.6], teal: [0.24, 0.72, 0.67], orange: [0.96, 0.65, 0.26] };
  for (const scene of [WIDE, TALL]) {
    const canvas = new MockCanvas();
    const hero = await ascii.startHero(canvas, { scene, palette });
    await new Promise((r) => setTimeout(r, 40));
    hero.setPalette({ ...palette, laneMuted: [0.5, 0.5, 0.5] });
    hero.setPaused(true);
    hero.dispose();
  }
});

// ---- the ASCII web (full bleed) and B's hub lamp ----

const webModel = await import("./web-model.ts");
const webLight = await import("./web-light.ts");
const webMod = await import("./web.ts");

for (const [name, l] of Object.entries(webModel.WEB)) {
  test(`web ${name}: every line is one task through the hub, in reply-contract rhythm`, () => {
    const web = webModel.createWeb(l);
    web.advance(120);
    const lines = web.lines();
    assert.ok(lines.length > 20, `${lines.length} lines`);
    const ids = new Set(l.agents.map((a) => a.id));
    for (const x of lines) {
      assert.ok(ids.has(x.from) && ids.has(x.to), "named agents at both ends");
      const kinds = x.pulses.map((p) => p.kind);
      assert.equal(kinds.filter((k) => k === "task").length, 1, "one teal task out");
      assert.equal(kinds.filter((k) => k === "result").length, 1, "exactly one result");
      assert.ok(kinds.filter((k) => k === "progress").length >= 2, "progress pulses back");
      assert.equal(kinds[0], "task");
      assert.equal(kinds.at(-1), "result", "the result comes last");
      for (const p of x.pulses) assert.equal(p.dir, p.kind === "task" ? 1 : -1, "sent goes out, received comes back");
      const atHub = webModel.pointAt(x, x.hubS);
      assert.ok(Math.hypot(atHub[0] - l.hub.cx, atHub[1] - l.hub.cy) < 1, "the line passes through hub-server");
      assert.equal(webModel.aliveAt(x, x.dies + 2), 0, "the line dies");
    }
  });

  test(`web ${name}: a calm number of lines alive, always moving`, () => {
    const web = webModel.createWeb(l);
    const counts = [];
    for (let t = 20; t < 120; t += 0.5) counts.push(web.live(t).filter((x) => webModel.aliveAt(x, t) > 0.5).length);
    const max = Math.max(...counts);
    const mean = counts.reduce((s, c) => s + c, 0) / counts.length;
    assert.ok(max <= l.maxLines, `max ${max}`);
    assert.ok(mean >= l.maxLines * 0.4 && mean <= 6, `mean ${mean.toFixed(2)}`);
    assert.ok(Math.min(...counts) >= 1, "never empty");
  });

  test(`web ${name}: poster light puts each pulse in its own colour, and the uniform fits the shader`, () => {
    const web = webModel.createWeb(l);
    const t = 47.3;
    const frame = webModel.frameAt(l, web.live(t), t);
    assert.ok(frame.glow > 0 && frame.glow < 1);
    assert.equal(frame.events.length, 12);
    assert.equal(frame.ripples.length, webModel.RIPPLES);
    const light = webLight.cellLight(l, frame);
    const [cols, rows] = l.grid;
    assert.equal(light.length, cols * rows * 4);
    for (const line of frame.lines) {
      for (const [s, amp] of line.pulses) {
        const x = web.live(t).find((y) => y.pts === line.pts);
        if (s < 0 || s > x.length) continue;
        const [px, py] = webModel.pointAt(x, s);
        const i = (Math.min(rows - 1, Math.floor(py / (l.height / rows))) * cols + Math.min(cols - 1, Math.floor(px / (l.width / cols)))) * 4;
        if (light[i + 3] < 0.5) continue; // under the copy the field yields
        if (amp > 0) assert.ok(light[i] > 0.1, "teal where a sent pulse is");
        else assert.ok(light[i + 1] > 0.1, "orange where a received pulse is");
      }
    }
    const u = webMod.webUniform(l, frame);
    assert.equal(u.line_info.length, 6);
    assert.equal(u.pts.length, 48);
    assert.equal(u.pulses.length, 36);
    assert.equal(u.ripples.length, 8);
    assert.equal(u.clicks.length, 4);
    assert.equal(u.mask.length, 4);
    assert.equal(u.mask2.length, 4);
    const rows2 = webLight.webRows(cols, rows);
    assert.equal(rows2.length, rows);
    assert.ok(rows2.every((r) => r.length === cols));
  });
}

test("B's hub lamp: rim flashes, neutral glow and ripples for every envelope", () => {
  const layout = wideLayout(WIDE);
  const traffic = createTraffic(layout, 7, { through: true });
  const lamp = hubLamp(traffic.live(POSTER_T), POSTER_T);
  assert.equal(lamp.ripples.length, 6);
  assert.ok(lamp.ripples.some((r) => r[1] > 0), "something is rippling");
  for (const [radius, amp] of lamp.ripples) assert.ok(radius >= 0 && amp >= 0 && amp <= 1);
});

test("the web starts, recolours and disposes on the mock adapter", async () => {
  const palette = { lane: [0, 0, 0], laneRaised: [0, 0, 0], laneLine: [0, 0, 0], laneInk: [1, 1, 1], laneMuted: [0.6, 0.6, 0.6], teal: [0.24, 0.72, 0.67], orange: [0.96, 0.65, 0.26] };
  for (const band of Object.keys(webModel.WEB)) {
    const canvas = new MockCanvas();
    const hero = await webMod.startWeb(canvas, { band, palette });
    await new Promise((r) => setTimeout(r, 40));
    hero.setPalette({ ...palette, laneInk: [0.9, 0.9, 0.9] });
    hero.setPaused(true);
    hero.dispose();
  }
});

// ---- round 4: bands, safe zones, the copy mask, click rings ----

/** Realistic (hero aspect, hero height) pairs per band, from the hero's min-height rules and the band queries. */
const CASES = {
  P: [0.75, 1.0, 1.3].flatMap((a) => [920, 960, 1000].map((h) => [a, h])),
  M: [1.25, 1.6, 1.9].flatMap((a) => [760, 870, 980].map((h) => [a, h])).concat([[2.2, 980], [2.6, 980]]),
  W: [1.75, 2.06, 2.4, 2.65].flatMap((a) => [760, 870, 980].map((h) => [a, h])),
  tall: [0.42, 0.45, 0.5].flatMap((a) => [860, 940].map((h) => [a, h])),
};
const HEADER = 88;
const STRIP = { P: 116, M: 116, W: 116, tall: 124 };

for (const band of ["P", "M", "W", "tall"]) {
  const l = webModel.WEB[band];
  test(`web ${band}: endpoints and the hub stay in safe zones of the real hero box`, () => {
    for (const [aspect, h] of CASES[band]) {
      const w = aspect * h;
      const m = webModel.cover(l, w, h);
      const px = (x, y) => [x * m.k + m.x, y * m.k + m.y];
      for (const a of l.agents) {
        const [x, y] = px(a.x, a.y);
        assert.ok(x > 12 && x < w - 12, `${a.id} on screen at ${w.toFixed(0)}×${h}`);
        assert.ok(y - 8 > HEADER && y + 8 < h - STRIP[band], `${a.id} clear of header and strip at ${w.toFixed(0)}×${h} (y ${y.toFixed(0)})`);
      }
      const legend = px(l.hub.cx, l.plate.y + l.plate.h + (band === "tall" ? 12 : 14) * 3.6)[1];
      assert.ok(legend < h - STRIP[band], `hub legend clear of the strip at ${w.toFixed(0)}×${h}`);
    }
    const c = l.copy;
    const pad = 24 / 0.9;
    for (const a of l.agents) assert.ok(a.x < c[0] - pad || a.x > c[2] + pad || a.y < c[1] - pad || a.y > c[3] + pad, `${a.id} clear of the copy`);
    assert.ok(l.plate.x > l.copy[2] || l.plate.y > l.copy[3], "the hub sits beside or below the copy");
  });
}

test("the field yields around the copy, with a smooth feather and no edge", () => {
  const m = [100, 100, 500, 300, webModel.FEATHER];
  assert.equal(webLight.yieldAt(300, 200, m), 0, "inside: yields fully");
  assert.equal(webLight.yieldAt(300, 300 + webModel.FEATHER + 1, m), 1, "beyond the feather: untouched");
  let prev = 0;
  for (let y = 300; y <= 300 + webModel.FEATHER; y += 5) {
    const v = webLight.yieldAt(300, y, m);
    assert.ok(v >= prev && v - prev < 0.06, "monotone, no step");
    prev = v;
  }
  const l = webModel.WEB.M;
  const lines = webModel.createWeb(l).live(POSTER_T);
  const open = webLight.cellLight(l, webModel.frameAt(l, lines, POSTER_T, { copy: [-1, -1, -1, -1] }));
  const covered = webLight.cellLight(l, webModel.frameAt(l, lines, POSTER_T, { copy: [0, 0, l.width, l.height] }));
  for (let i = 0; i < open.length; i += 4) for (let c = 0; c < 3; c++) assert.ok(covered[i + c] <= open[i + c] * webLight.YIELD.floor + 1e-6);
});

test("click rings: neutral, at most four, oldest dropped, gone in about 1.5 s", () => {
  const clicks = [0, 0.1, 0.2, 0.3, 0.4].map((t, i) => ({ x: 100 * i, y: 50, t }));
  const rings = webModel.clickRings(clicks, 0.5);
  assert.equal(rings.length, webModel.CLICKS);
  assert.ok(!rings.some((r) => r[0] === 0 && r[3] > 0), "the oldest (x = 0) was dropped");
  assert.deepEqual(rings.map((r) => r[0]), [100, 200, 300, 400]);
  for (let i = 1; i < 4; i++) assert.ok(rings[i - 1][2] > rings[i][2], "older rings are wider");
  assert.ok(webModel.clickRings([{ x: 0, y: 0, t: 0 }], 1.4)[0][3] < 0.07, "faded by 1.4 s");
  assert.equal(webModel.clickRings([{ x: 0, y: 0, t: 0 }], 1.6)[0][3], 0, "gone by 1.6 s");
  const l = webModel.WEB.W;
  const frame = webModel.frameAt(l, [], 10, { copy: [-1, -1, -1, -1], clicks: webModel.clickRings([{ x: 1200, y: 600, t: 9.7 }], 10) });
  const light = webLight.cellLight(l, frame);
  let sent = 0, recv = 0, neutral = 0;
  for (let i = 0; i < light.length; i += 4) { sent += light[i]; recv += light[i + 1]; neutral += light[i + 2]; }
  assert.ok(neutral > 1 && sent === 0 && recv === 0, "a click is neither sent nor received");
});

test("cover mapping: client point → design → device pixel round-trips", () => {
  const l = webModel.WEB.W;
  for (const [w, h, dpr] of [[1879, 920, 1], [2545, 980, 1.5], [1265, 760, 1.5]]) {
    const css = webModel.cover(l, w, h);
    const dev = webModel.cover(l, w * dpr, h * dpr);
    for (const [cx, cy] of [[w * 0.7, h * 0.8], [40, h - 30]]) {
      const ux = (cx - css.x) / css.k, uy = (cy - css.y) / css.k;
      assert.ok(Math.abs(ux * dev.k + dev.x - cx * dpr) < 1e-6 && Math.abs(uy * dev.k + dev.y - cy * dpr) < 1e-6);
    }
  }
});

// ---- lightweight: the external poster document, the line early-out, the pixel budget ----

const { readFileSync } = await import("node:fs");
const posterDoc = await import("./web-poster-svg.ts");

test("posters/web.svg is up to date (run tools/bake-web.mjs after changing the web)", () => {
  const committed = readFileSync(new URL("./posters/web.svg", import.meta.url), "utf8");
  assert.equal(committed, posterDoc.posterSvg());
  for (const id of ["field-wide", "field-tall", "lit-P", "lit-M", "lit-W", "lit-tall"]) assert.ok(committed.includes(`id="${id}"`), id);
  assert.ok(!committed.includes("var(--"), "literal colours: an external document cannot see page variables");
});

test("a line's light stays inside its inflated box", () => {
  const l = webModel.WEB.W;
  const t = 52;
  const frame = webModel.frameAt(l, webModel.createWeb(l).live(t), t, { copy: [-1, -1, -1, -1] });
  const sigma = webLight.lineSigma(l);
  assert.ok(Math.exp(-((4 * sigma) ** 2) / (2 * sigma ** 2)) < 4e-4, "4 sigma is beyond any visible light");
  for (const line of frame.lines) {
    const [x0, y0, x1, y1] = webLight.lineBox(line.pts, l);
    for (let j = 0; j < line.pts.length; j += 2) assert.ok(line.pts[j] - x0 >= 4 * sigma - 1e-6 && x1 - line.pts[j] >= 4 * sigma - 1e-6 && line.pts[j + 1] - y0 >= 4 * sigma - 1e-6 && y1 - line.pts[j + 1] >= 4 * sigma - 1e-6);
  }
});

test("the canvas keeps to its pixel budget", () => {
  assert.equal(webMod.dprCap(1425, 900), 1.5);
  assert.ok(Math.abs(webMod.dprCap(2545, 980) * webMod.dprCap(2545, 980) * 2545 * 980 - webMod.PIXEL_BUDGET) < 1);
  assert.equal(webMod.dprCap(5000, 2000), 1);
});
