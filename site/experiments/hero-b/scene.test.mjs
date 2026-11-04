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

const webTracks = await import("./web-tracks.ts");
const { placeAlpha } = await import("./web-places.ts");
const { viewMap } = await import("./light.ts");
const near = (x, y, px, py, eps) => Math.hypot(x - px, y - py) < eps;

for (const [name, l] of Object.entries(webModel.WEB)) {
  const byTask = (traffic) => {
    const tasks = new Map();
    for (const m of traffic.pending()) tasks.set(m.task, [...(tasks.get(m.task) ?? []), m]);
    return [...tasks.values()];
  };

  test(`web ${name}: every task follows the reply contract, place to place through the hub`, () => {
    const traffic = webTracks.createWebTraffic(l);
    traffic.advance(150);
    const tasks = byTask(traffic);
    assert.ok(tasks.length > 40, `${tasks.length} tasks`);
    const agent = new Map(l.agents.map((a) => [a.id, a]));
    for (const msgs of tasks) {
      const [task, ...back] = msgs;
      const orch = agent.get(task.from), worker = agent.get(task.to);
      assert.equal(task.kind, "task");
      assert.equal(orch.role, "orchestrator");
      assert.equal(worker.role, "worker");
      assert.notEqual(orch.slot, worker.slot, "between two places");
      assert.equal(back[0].kind, "status");
      assert.equal(back[0].detail, "working");
      assert.equal(back[1].detail, "started");
      assert.ok(back.slice(2, -1).length >= 1 && back.slice(2, -1).every((m) => m.kind === "event" && m.detail.startsWith("progress")));
      assert.equal(back.at(-1).kind, "result", "exactly one result, last");
      for (const m of msgs) assert.equal(m.channel, m.kind === "task" ? 0 : 1, "teal goes out, orange comes back");
      for (const m of back) assert.ok(m.from === worker.id && m.to === orch.id);
      for (let i = 1; i < msgs.length; i++) assert.ok(msgs[i].t0 > msgs[i - 1].t0);
      const end = traffic.arrival(back.at(-1)) + 8;
      for (let t = task.t0; t <= end; t += 0.25)
        for (const a of [orch, worker]) assert.equal(placeAlpha(a.slot, l.slots[a.slot], a.j, t), 1, `${a.place} on screen while ${task.task} glows`);
    }
  });

  test(`web ${name}: never quiet for long, and every place gets traffic`, () => {
    const traffic = webTracks.createWebTraffic(l);
    const slot = new Map(l.agents.map((a) => [a.id, a.slot]));
    const lit = new Set();
    let quiet = 0;
    for (let t = 15; t < 400; t += 0.25) {
      const live = traffic.live(t);
      for (const m of live) lit.add(slot.get(m.from)).add(slot.get(m.to));
      // Between messages the exposure still glows (tau 5.5 s); a lull never lasts long.
      quiet = live.some((m) => traffic.arrival(m) >= t) ? 0 : quiet + 0.25;
      assert.ok(quiet <= 2.5, `nothing in flight for ${quiet} s at ${t}`);
    }
    assert.equal(lit.size, l.slots.length, "every slot carries traffic");
  });

  test(`web ${name}: two lanes per agent, into the plate and through the hub`, () => {
    const t = webTracks.trackLayout(l);
    const half = l.plate.w / 2;
    for (const a of l.agents) {
      const { toHub, fromHub } = t.lanes[a.id];
      const end = toHub.pts.slice(-2);
      const onPlate = Math.max(Math.abs(end[0] - l.hub.cx), Math.abs(end[1] - l.hub.cy));
      assert.ok(Math.abs(onPlate - half) < t.gap + 0.5, `${a.id} enters the plate`);
      assert.ok(near(toHub.pts[0], toHub.pts[1], a.x, a.y, t.gap * 1.5), `${a.id}: lane starts at its port`);
      const mid = Math.floor(toHub.pts.length / 4) * 2;
      const back = fromHub.pts.length - 2 - mid;
      const apart = Math.hypot(toHub.pts[mid] - fromHub.pts[back], toHub.pts[mid + 1] - fromHub.pts[back + 1]);
      assert.ok(Math.abs(apart - 2 * t.gap) < 0.5, `${a.id}: lanes ${apart.toFixed(2)} apart`);
    }
    const traffic = webTracks.createWebTraffic(l);
    traffic.advance(60);
    for (const m of traffic.pending().slice(0, 30)) {
      const a = m.a.pts, mid = m.mid.pts, b = m.b.pts;
      assert.ok(near(a[a.length - 2], a[a.length - 1], mid[0], mid[1], 1e-3) && near(mid[mid.length - 2], mid[mid.length - 1], b[0], b[1], 1e-3), "continuous through the hub");
    }
  });

  test(`web ${name}: the ground frame fits its shader, and the field is the web's own traffic`, () => {
    const traffic = webTracks.createWebTraffic(l);
    const t = 47.3;
    const frame = webModel.groundFrame(l, traffic.live(t), t);
    assert.ok(frame.glow > 0 && frame.glow < 1);
    assert.equal(frame.events.length, 12);
    assert.equal(frame.ripples.length, webModel.RIPPLES);
    assert.ok(frame.ripples.some((r) => r[1] > 0), "something is rippling");
    const [cols, rows] = l.grid;
    assert.equal(webLight.cellLight(l, frame).length, cols * rows * 2);
    const u = webMod.webUniform(l, frame);
    assert.equal(u.ripples.length, 8);
    assert.equal(u.clicks.length, 4);
    assert.equal(u.mask.length, 4);
    assert.equal(u.mask2.length, 4);
    const text = webLight.webRows(cols, rows);
    assert.equal(text.length, rows);
    assert.ok(text.every((r) => r.length === cols));
  });

  test(`web ${name}: the tracks' light lands where the field and the overlay draw (same cover map)`, () => {
    for (const [w, h] of [[1440, 900], [2560, 980], [768, 1000], [390, 860], [3000, 900]]) {
      const v = viewMap(l, w, h, true);
      const c = webModel.cover(l, w, h);
      assert.ok(Math.abs(v.scale - c.k) < 1e-9 && Math.abs(v.off[0] - c.x) < 1e-9 && Math.abs(v.off[1] - c.y) < 1e-9);
    }
  });
}

test("B draws its own design space: without cover the view is the identity", () => {
  const v = viewMap({ width: 1200, height: 440 }, 2400, 880, false);
  assert.deepEqual([v.scale, v.off[0], v.off[1], v.size[0], v.size[1]], [1, 0, 0, 1200, 440]);
});

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
  P: [0.75, 1.1, 1.45].flatMap((a) => [920, 960, 1000].map((h) => [a, h])),
  M: [1.38, 1.6, 1.95].flatMap((a) => [760, 870, 980].map((h) => [a, h])).concat([[2.3, 980], [2.6, 980]]),
  W: [1.85, 2.06, 2.4, 2.65].flatMap((a) => [760, 870, 980].map((h) => [a, h])),
  tall: [0.42, 0.45, 0.5].flatMap((a) => [860, 940].map((h) => [a, h])),
};
const HEADER = 88;
const STRIP = { P: 116, M: 116, W: 116, tall: 124 };
const { CARD, PLACES } = await import("./web-places.ts");

/** A place card's box in design units: its name above, one row per agent, names on the align side. */
function cardBox(l, slot, pid) {
  const card = CARD[l.variant];
  const agents = PLACES[pid].agents.slice(0, card.rows);
  const nameW = Math.max(...agents.map(([id]) => id.length)) * card.name * 0.6;
  const titleW = pid.length * card.title * 0.6;
  const start = slot.align === "start";
  const x0 = start ? slot.x - 6 : Math.min(slot.x - 11 - nameW, slot.x + 5 - titleW);
  const x1 = start ? Math.max(slot.x + 11 + nameW, slot.x - 5 + titleW) : slot.x + 6;
  return [x0, slot.y - card.title * 0.8, x1, slot.y + card.first + (agents.length - 1) * card.row + card.name * 0.45];
}
const overlaps = (a, b, pad = 0) => a[0] < b[2] + pad && a[2] > b[0] - pad && a[1] < b[3] + pad && a[3] > b[1] - pad;

for (const band of ["P", "M", "W", "tall"]) {
  const l = webModel.WEB[band];
  const boxes = l.slots.filter((slot) => !slot.reach).flatMap((slot) => slot.places.map((pid) => ({ pid, slot, box: cardBox(l, slot, pid) })));
  const size = band === "tall" ? 12 : 14;
  const hubBox = [l.plate.x - 6, l.plate.y - 6, l.plate.x + l.plate.w + 6, l.plate.y + l.plate.h + size * 3.8];
  const legendHalf = 10.5 * size * 0.9 * 0.6;
  hubBox[0] = Math.min(hubBox[0], l.hub.cx - legendHalf);
  hubBox[2] = Math.max(hubBox[2], l.hub.cx + legendHalf);

  test(`web ${band}: place cards and the hub stay in safe zones of the real hero box`, () => {
    for (const [aspect, h] of CASES[band]) {
      const w = aspect * h;
      const m = webModel.cover(l, w, h);
      const px = (x, y) => [x * m.k + m.x, y * m.k + m.y];
      for (const { pid, box } of boxes) {
        const [x0, y0] = px(box[0], box[1]);
        const [x1, y1] = px(box[2], box[3]);
        const at = `${pid} at ${w.toFixed(0)}×${h}`;
        assert.ok(x0 > 8 && x1 < w - 8, `${at}: on screen (${x0.toFixed(0)}..${x1.toFixed(0)} of ${w.toFixed(0)})`);
        assert.ok(y0 > HEADER && y1 < h - STRIP[band], `${at}: clear of header and strip (${y0.toFixed(0)}..${y1.toFixed(0)})`);
      }
      assert.ok(px(0, hubBox[3])[1] < h - STRIP[band], `hub legend clear of the strip at ${w.toFixed(0)}×${h}`);
    }
  });

  test(`web ${band}: cards keep clear of the copy, the hub and each other`, () => {
    const copy = l.copy;
    for (const { pid, box } of boxes) {
      assert.ok(!overlaps(box, copy, 24 / 0.9), `${pid} clear of the copy`);
      assert.ok(!overlaps(box, hubBox, 8), `${pid} clear of the hub and its labels`);
    }
    // Cards in the same slot take turns; cards in different slots must not touch.
    for (const a of boxes) for (const b of boxes) if (a.slot !== b.slot) assert.ok(!overlaps(a.box, b.box, 6), `${a.pid} vs ${b.pid}`);
    assert.ok(l.plate.x > l.copy[2] || l.plate.y > l.copy[3], "the hub sits beside or below the copy");
  });

  const reaches = l.slots.map((slot, i) => ({ slot, i })).filter((r) => r.slot.reach);
  if (band === "M" || band === "W")
    test(`web ${band}: three reaches leave the hub's upper left, behind the headline and off the left edge`, () => {
      assert.equal(reaches.length, 3);
      const t = webTracks.trackLayout(l);
      const [x0, y0, x1, , head] = l.copy;
      for (const { slot, i } of reaches) {
        const m = t.machines[i];
        assert.ok(Math.abs(m.entry[0] - l.plate.x) < 1e-6 && m.entry[1] < l.hub.cy && m.entry[1] > l.plate.y + l.plate.r - 2, `${slot.places[0]}: out of the plate's left face, upper half`);
        for (const [aspect, h] of CASES[band]) {
          const c = webModel.cover(l, aspect * h, h);
          for (const p of [m.gate, ...m.agents.map((a) => [a.x, a.y])]) assert.ok(p[0] * c.k + c.x < -4, `${slot.places[0]}: gate and ports beyond the frame at ${aspect}×${h}`);
        }
        const trunk = webTracks.railPath(l, i, 0);
        const pts = t.lanes[m.agents[0].id].toHub.pts;
        let behind = 0;
        for (let k = 0; k < pts.length; k += 2) {
          const [x, y] = [pts[k], pts[k + 1]];
          if (x > x0 && x < x1 && y > y0 && y < head) behind++;
          if (x > x0 && x < x1) assert.ok(y < head + 4, `${slot.places[0]}: stays above the paragraph (y ${y.toFixed(0)} at x ${x.toFixed(0)})`);
          for (const { pid, box } of boxes) assert.ok(!(x > box[0] - 12 && x < box[2] + 12 && y > box[1] - 12 && y < box[3] + 12), `${slot.places[0]} clear of ${pid}`);
        }
        assert.ok(behind > 20, `${slot.places[0]}: runs behind the headline`);
        assert.ok(trunk.startsWith("M") && !trunk.includes("NaN"));
      }
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
  const msgs = webTracks.createWebTraffic(l).live(POSTER_T);
  const clicks = webModel.clickRings([{ x: 900, y: 500, t: POSTER_T - 0.3 }], POSTER_T);
  const open = webLight.cellLight(l, webModel.groundFrame(l, msgs, POSTER_T, { copy: [-1, -1, -1, -1], clicks }));
  const covered = webLight.cellLight(l, webModel.groundFrame(l, msgs, POSTER_T, { copy: [-100, -100, l.width + 100, l.height + 100], clicks }));
  assert.ok(open.some((v, i) => i % 2 === 0 && v > 0.1), "rings light the open field");
  for (let i = 0; i < open.length; i += 2) assert.ok(covered[i] <= open[i] * webLight.YIELD.floor * (1 + 1e-5) + 1e-6);
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
  const frame = webModel.groundFrame(l, [], 10, { copy: [-1, -1, -1, -1], clicks: webModel.clickRings([{ x: 1200, y: 600, t: 9.7 }], 10) });
  const light = webLight.cellLight(l, frame);
  let neutral = 0;
  for (let i = 0; i < light.length; i += 2) neutral += light[i];
  assert.ok(neutral > 1, "a click rings through the ground (neutral: the tracks carry the colour)");
  assert.equal(frame.events.filter((e) => e[2] < 99).length, 0, "and never flashes the hub");
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
  for (const id of ["field-wide", "field-tall", "hub-P", "hub-M", "hub-W", "hub-tall"]) assert.ok(committed.includes(`id="${id}"`), id);
  for (const [band, l] of Object.entries(webModel.WEB))
    l.slots.forEach((slot, i) => slot.places.forEach((_, j) => {
      if (slot.reach) assert.ok(!committed.includes(`id="card-${band}-${i}-${j}"`), `card-${band}-${i}-${j}: a reach has no card`);
      else assert.ok(committed.includes(`id="card-${band}-${i}-${j}"><text`), `card-${band}-${i}-${j}: text only (the label guard measures it)`);
      assert.ok(committed.includes(`<path id="rail-${band}-${i}-${j}" d="M`), `rail-${band}-${i}-${j}`);
    }));
  assert.ok(!committed.includes("var(--"), "literal colours: an external document cannot see page variables");
  for (const band of ["P", "M", "W", "tall"]) assert.ok(readFileSync(new URL(`./posters/web-trails-${band}.png`, import.meta.url)).length < 60_000, `web-trails-${band}.png stays small`);
});

test("the canvas keeps to its pixel budget", () => {
  assert.equal(webMod.dprCap(1425, 900), 1.5);
  assert.ok(Math.abs(webMod.dprCap(2545, 980) * webMod.dprCap(2545, 980) * 2545 * 980 - webMod.PIXEL_BUDGET) < 1);
  assert.equal(webMod.dprCap(5000, 2000), 1);
});
