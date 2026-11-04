// Unit tests for the visualizer provenance label (visualizer/js/model.js).
// Run with: node tests/e2e_visualizer/origin_label.test.mjs
// (also run by run_smoke.sh). model.js imports state.js, which reads
// window.matchMedia at load, so stub it before the dynamic import.
import assert from 'node:assert/strict';

globalThis.window = { matchMedia: () => ({ matches: false }) };
const { originLabel } = await import('../../visualizer/js/model.js');

// A self-reported value carrying the separator stays one self-reported
// part; a `~`-prefixed user does not look self-reported.
{
  const label = originLabel({
    user: '~alice',
    reported: { model: 'x · alice · claude-code', session: 'ab·~cdefghij' },
    on_behalf_of: { identity: 'id·~x', user: 'bob · ~mallory' },
  }, true);
  assert.equal(
    label,
    '-alice · ~x - alice - claude-code · ~session:ab--cdef'
      + ' · on behalf of bob - -mallory',
  );
  assert.equal(label.split(' · ').length - 1, 3);
  assert.equal([...label].filter((c) => c === '~').length, 2);
}

// Bidi overrides/isolates, zero-width characters, the BOM and the
// line/paragraph separators are dropped (browsers apply bidi to text
// nodes too).
{
  const label = originLabel({
    user: 'al\u202eice\u200b\u2066\ufeff\u2060\u2028\u2029\u200f',
    reported: { model: 'mo\u202adel\u00ad\u061c\u{e0061}' },
  });
  assert.equal(label, 'alice · ~model');
  assert.ok(!/[\p{Cc}\p{Cf}\u2028\u2029]/u.test(label), label);
}

// The 64-character cap counts code points, not UTF-16 code units: an
// astral character at the boundary is kept whole, never split.
{
  const model = 'a'.repeat(63) + '\u{1f600}' + 'b'.repeat(10);
  const label = originLabel({ reported: { model } });
  assert.equal(label, '~' + 'a'.repeat(63) + '\u{1f600}');
  assert.equal(Array.from(label).length, 65);
}

console.log('origin label unit tests: ok');
