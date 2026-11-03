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
