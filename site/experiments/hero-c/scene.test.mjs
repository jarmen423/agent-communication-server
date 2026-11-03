// Concept C under vgpu's mock adapter: the script keeps the reply contract, the
// poster moment shows a full delegation and a wave in flight, and the GPU layer
// builds, compiles and encodes frames for both stages. Run from site/:
//   node experiments/hero-c/scene.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { prepareShader } from "@vgpu/wgsl/prepare";
import { clock, frame, init, target } from "vgpu/mock";
import { register } from "node:module";

// Plain `node` can't load .ts on this Node (22.22, no default type stripping):
// a tiny loader hook transpiles our two TypeScript modules with the site's
// TypeScript, then they are imported dynamically.
register(
  "data:text/javascript," +
    encodeURIComponent(`
      import ts from "${import.meta.resolve("typescript")}";
      import { readFileSync } from "node:fs";
      import { fileURLToPath } from "node:url";
      export async function load(url, context, next) {
        if (!url.endsWith(".ts")) return next(url, context);
        const src = readFileSync(fileURLToPath(url), "utf8");
        const out = ts.transpileModule(src, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022, verbatimModuleSyntax: false } });
        return { format: "module", source: out.outputText, shortCircuit: true };
      }
    `),
  import.meta.url,
);
const { COLOR, KIND, MAX_MARKS, PERIOD, STRIDE, T0, TALL_TAPE, TASKS, WIDE_TAPE, buildMarks, clockText, packMarks, tapeLabels } = await import("./schedule.ts");
const { createTape } = await import("./tape.ts");

const shader = (name) => prepareShader(readFileSync(new URL(name, import.meta.url), "utf8"));
const rgb = (hex) => [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
const PALETTE = {
  lane: rgb("171a21"), laneRaised: rgb("1f232c"), laneLine: rgb("2c313b"), laneInk: rgb("eef0ea"),
  laneMuted: rgb("a3a9b0"), teal: rgb("3cb8ab"), orange: rgb("f5a742"),
};

test("every leg of the script: progress first, then exactly one result", () => {
  for (const task of TASKS) {
    for (const leg of task.legs) {
      const progress = [...leg.status, ...leg.events];
      assert.ok(progress.length >= 2, `${task.channel} → ${leg.worker} streams progress`);
      assert.ok(Math.min(...progress) > task.sent, "progress comes after the task is sent");
      assert.ok(Math.max(...progress) < leg.result, "the result is the last envelope of the leg");
      assert.equal(typeof leg.result, "number", "one result per leg");
      assert.ok(leg.result < PERIOD, "the loop closes before the next cycle");
    }
  }
});

test("the poster moment: a full delegation and a wave still in flight", () => {
  const sentChannel = "#task.7f3a2c1e";
  const task = TASKS.find((t) => t.channel === sentChannel);
  assert.ok(task.sent < T0 && task.legs[0].result < T0, "me → claude-1 is complete at T0");
  const wave = TASKS.find((t) => t.channel.startsWith("#wave."));
  assert.equal(wave.legs.length, 2, "the wave fans out to two workers");
  assert.ok(wave.sent < T0 && wave.legs.every((l) => l.result > T0), "the wave is in flight at T0");

  for (const L of [WIDE_TAPE, TALL_TAPE]) {
    const marks = buildMarks(T0, L, false);
    assert.ok(marks.length > 20 && marks.length <= MAX_MARKS);
    const tiles = (k) => marks.filter((m) => m.kind === k);
    assert.ok(tiles(KIND.sentTile).every((m) => m.color === COLOR.teal), "sent is always teal");
    assert.ok(tiles(KIND.recvTile).every((m) => m.color === COLOR.orange), "received is always orange");
    assert.ok(tiles(KIND.recvTile).length >= 1, "at least one result is on the tape");
    for (const m of marks) {
      assert.ok(m.cx + m.hw <= L.play + 12, "nothing is written past the pen");
      assert.ok(m.cy - m.hh >= 0 && m.cy + m.hh <= L.height, "marks stay inside the stage");
    }
    const labels = tapeLabels(T0, L);
    assert.ok(labels.events.some((l) => l.text === sentChannel), "the task channel rides on the tape");
    assert.ok(labels.ticks.some((l) => l.text === clockText(20)), "time ticks print real clock times");
  }
});

test("the tape loops without gaps or overflow", () => {
  for (const L of [WIDE_TAPE, TALL_TAPE]) {
    const out = new Float32Array(MAX_MARKS * STRIDE);
    for (let t = T0; t < T0 + PERIOD * 2; t += 0.25) {
      const n = packMarks(buildMarks(t, L), out);
      assert.ok(n > 0 && n <= MAX_MARKS, `marks at t=${t}`);
      assert.ok(buildMarks(t, L).length < MAX_MARKS, "never truncated");
    }
  }
});

test("the GPU layer builds, compiles and encodes frames (mock adapter)", async () => {
  const gpu = await init();
  for (const L of [WIDE_TAPE, TALL_TAPE]) {
    const colour = target(gpu, { size: [L.width / 4, L.height / 4], clearColor: [0, 0, 0, 0] });
    const tape = createTape(gpu, { paper: shader("./paper.wgsl"), marks: shader("./marks.wgsl") }, L, PALETTE);
    await tape.compile(colour);
    const time = clock(gpu);
    for (let i = 0; i < 30; i++) {
      time.advance(1 / 60);
      const now = T0 + time.time;
      const n = packMarks(buildMarks(now, L), tape.items);
      assert.ok(n > 0);
      frame(gpu, (f) => f.pass(colour, (pass) => tape.draw(pass, now, 0.25, n)));
    }
    tape.setPalette({ ...PALETTE, teal: rgb("0f7c74"), orange: rgb("d9731a") });
    frame(gpu, (f) => f.pass(colour, (pass) => tape.draw(pass, T0, 0.25, 0)));
    tape.dispose();
  }
  gpu.dispose();
});
