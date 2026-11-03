// Concept A tests. Run from site/:  node experiments/hero-a/scene.test.mjs
//
// 1. The script keeps the brand's and the protocol's promises over a whole loop.
// 2. vgpu's mock adapter accepts every uniform the browser would upload (shapes
//    are checked against the shader's reflection), for both stages.
// 3. A real Dawn render (llvmpipe here) puts teal and orange where they belong.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { register } from "node:module";
import { test } from "node:test";

register("./node-hooks.mjs", import.meta.url);
const { WIDE, TALL } = await import("../../components/hero/geometry.ts");
const { buildModel, frameAt, PERIOD, POSTER_T, TASKS } = await import("./script.ts");
const { boardValues, liveValues } = await import("./board.ts");
const { prepareShader } = await import("@vgpu/wgsl/prepare");

const shader = prepareShader(readFileSync(new URL("./signal.wgsl", import.meta.url), "utf8"));
const PALETTE = {
  lane: [0.09, 0.1, 0.13], laneRaised: [0.14, 0.16, 0.2], laneLine: [0.2, 0.22, 0.25], laneInk: [0.93, 0.94, 0.92],
  laneMuted: [0.64, 0.66, 0.69], teal: [60 / 255, 184 / 255, 171 / 255], orange: [245 / 255, 167 / 255, 66 / 255],
};
const times = (step = 1 / 60) => Array.from({ length: Math.ceil(PERIOD / step) }, (_, i) => i * step);

for (const scene of [WIDE, TALL]) {
  const name = scene.compact ? "tall" : "wide";
  const m = buildModel(scene);

  test(`${name}: teal only ever rides a sent track, orange only a received one`, () => {
    for (const t of times()) {
      for (const s of frameAt(m, t).sprites) assert.equal(s.dir, m.tracks[s.track].dir, `t=${t.toFixed(2)}`);
    }
  });

  test(`${name}: each task returns exactly one result tile, last, after its progress`, () => {
    for (const k of TASKS) {
      const kinds = k.envelopes.map((e) => e.kind);
      assert.equal(kinds.filter((x) => x === "result").length, 1);
      assert.equal(kinds.at(-1), "result");
      assert.equal(kinds[0], "task");
      assert.ok(kinds.includes("status") && kinds.includes("event"), "progress is streamed before the result");
    }
    // A result tile is on screen at some point for every task, and only on the received side.
    const seen = new Set();
    for (const t of times()) {
      for (const s of frameAt(m, t).sprites) if (s.kind === "tile" && s.dir === "received") seen.add(m.tracks[s.track].route);
    }
    for (const k of TASKS) assert.ok(seen.has(k.route), `result from ${k.worker} returns on ${k.route}`);
  });

  test(`${name}: the board fits the shader's arrays and lit runs stay on their tracks`, () => {
    for (const t of times()) {
      const f = frameAt(m, t);
      assert.ok(f.sprites.length <= 8, `t=${t.toFixed(2)}: ${f.sprites.length} sprites`);
      f.lit.forEach(([lo, hi], k) => {
        if (hi > lo) assert.ok(lo >= 0 && hi <= m.tracks[k].length + 1e-6, `track ${k} [${lo}, ${hi}]`);
      });
      assert.ok(f.hub.sent <= 1 && f.hub.received <= 1);
    }
  });

  test(`${name}: the loop repeats from a dark, calm board`, () => {
    for (const t of [0, PERIOD - 0.05]) {
      const f = frameAt(m, t);
      assert.equal(f.sprites.length, 0);
      assert.ok(f.lit.every(([lo, hi]) => hi <= lo), `dark at t=${t}`);
    }
    assert.deepEqual(frameAt(m, POSTER_T), frameAt(m, POSTER_T + PERIOD));
  });

  test(`${name}: mock adapter accepts the board and every frame of the loop`, async () => {
    const { init, effect, target } = await import("vgpu/mock");
    const gpu = await init();
    const errors = [];
    gpu.onError((e) => errors.push(e));
    const out = target(gpu, { size: [Math.round(scene.width / 10), Math.round(scene.height / 10)] });
    const fx = effect(gpu, shader, { set: { board: boardValues(m, PALETTE, 0.1), live: liveValues(frameAt(m, POSTER_T)) } });
    fx.compileSync(out);
    for (const t of times(0.25)) {
      fx.set({ live: liveValues(frameAt(m, t)) });
      fx.draw(out);
    }
    fx.set({ board: { teal: [1, 0, 0, 1], orange: [0, 0, 1, 1] } }); // a theme change is a partial update
    fx.draw(out);
    await gpu.settled();
    assert.deepEqual(errors, []);
    gpu.dispose();
  });
}

test("wide: a real render lights teal on sent, orange on received, and leaves labels alone", async (t) => {
  let vgpu;
  try {
    vgpu = await import("vgpu/node");
  } catch (err) {
    return t.skip(`no Dawn adapter: ${err.message}`);
  }
  const m = buildModel(WIDE);
  const gpu = await vgpu.init();
  const [w, h] = [WIDE.width, WIDE.height];
  const out = vgpu.target(gpu, { size: [w, h] });
  const fx = vgpu.effect(gpu, shader, { set: { board: boardValues(m, PALETTE, 1), live: liveValues(frameAt(m, POSTER_T)) } });
  fx.draw(out);
  const px = await out.color.read({ mipLevel: 0, region: "all" });
  const at = (x, y) => {
    const i = (Math.round(y) * w + Math.round(x)) * 4;
    return [...px.slice(i, i + 4)];
  };
  const hue = ([r, g, , a]) => (a < 40 ? "none" : g > r ? "teal" : "orange");
  const f = frameAt(m, POSTER_T);
  // Every sprite's centre is its direction's colour.
  for (const s of f.sprites) {
    const inCard = WIDE.nodes.some((n) => s.x > n.x && s.x < n.x + n.w && s.y > n.y && s.y < n.y + n.h);
    if (!inCard) assert.equal(hue(at(s.x, s.y)), s.dir === "sent" ? "teal" : "orange", `${s.kind} at ${s.x.toFixed(0)},${s.y.toFixed(0)}`);
  }
  // Route a's sent lamps are lit (teal), mid-lamp; the card text area stays untouched.
  const aSent = m.tracks[0];
  assert.equal(hue(at(aSent.pts[0][0] + 24 * 4 + 12, aSent.pts[0][1])), "teal");
  assert.equal(hue(at(60, 250)), "none", "nothing paints over me's card");
  assert.equal(hue(at(600, 220)), "none", "the lens of the mark is never painted");
  gpu.dispose();
});
