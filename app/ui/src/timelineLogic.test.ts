import assert from "node:assert/strict";
import { test } from "node:test";
import { gridMetrics, gridMove, rangeForViewport, sectionLayout } from "./gridLogic.ts";
import {
  PAGE, RowCache, absToEff, buildIndex, effRangeToAbs, effToAbs, rangeAdd, rangeCount, rangeHas, rangeRemoveIndex, rangeToggle,
  sectionOfAbs, sectionOfEff, type Range,
} from "./timelineLogic.ts";

const secs = [{ key: "a", count: 5 }, { key: "b", count: 3 }, { key: "c", count: 0 }, { key: "d", count: 4 }];

test("buildIndex: folded months keep their absolute slots but leave the effective list", () => {
  const ix = buildIndex(secs, new Set(["b"]));
  assert.deepEqual(ix.absStart, [0, 5, 8, 8]);
  assert.deepEqual(ix.effStart, [0, 5, 5, 5]);
  assert.equal(ix.absTotal, 12);
  assert.equal(ix.effTotal, 9);
});

test("effective <-> absolute index across a folded month and an empty one", () => {
  const ix = buildIndex(secs, new Set(["b"]));
  assert.equal(effToAbs(ix, 4), 4);
  assert.equal(effToAbs(ix, 5), 8); // first photo of d
  assert.equal(effToAbs(ix, 8), 11);
  assert.equal(sectionOfEff(ix, 5), 3);
  assert.equal(absToEff(ix, 6), -1); // inside the folded month
  assert.equal(absToEff(ix, 8), 5);
  assert.equal(sectionOfAbs(ix, 7), 1);
  const open = buildIndex(secs, new Set());
  for (let i = 0; i < open.effTotal; i++) assert.equal(absToEff(open, effToAbs(open, i)), i);
});

test("effRangeToAbs skips folded months and merges touching pieces", () => {
  const ix = buildIndex(secs, new Set(["b"]));
  assert.deepEqual(effRangeToAbs(ix, 3, 6), [[3, 4], [8, 9]]);
  const open = buildIndex(secs, new Set());
  assert.deepEqual(effRangeToAbs(open, 3, 9), [[3, 9]]);
  assert.deepEqual(effRangeToAbs(open, 7, 2), []);
  assert.deepEqual(effRangeToAbs(buildIndex([], new Set()), 0, 3), []);
});

test("ranges: add merges, toggle splits, counts and membership", () => {
  let rs: Range[] = [];
  rs = rangeAdd(rs, [[10, 20]]);
  rs = rangeAdd(rs, [[21, 25], [40, 41]]);
  assert.deepEqual(rs, [[10, 25], [40, 41]]);
  assert.equal(rangeCount(rs), 18);
  assert.ok(rangeHas(rs, 25) && rangeHas(rs, 40) && !rangeHas(rs, 26) && !rangeHas(rs, 9));
  rs = rangeToggle(rs, 15);
  assert.deepEqual(rs, [[10, 14], [16, 25], [40, 41]]);
  rs = rangeToggle(rs, 15);
  assert.deepEqual(rs, [[10, 25], [40, 41]]);
  assert.deepEqual(rangeRemoveIndex([[3, 3]], 3), []);
  assert.deepEqual(rangeAdd([[5, 9]], [[0, 100]]), [[0, 100]]);
});

test("uniform timeline grid: folded months take only their heading; keys cross months in the same column", () => {
  const ix = buildIndex(secs, new Set(["b"]));
  const g = gridMetrics(602); // 3 exact-size columns beside the preview dock
  const heads = secs.map(() => 44);
  const lay = sectionLayout(ix.effCount, heads, g, 14);
  assert.equal(lay.h[1], 44 + 6, "the folded month is its heading");
  // every tile's top is arithmetic: section top + heading + row * stride
  for (let si = 0; si < secs.length; si++) {
    const n = ix.effCount[si] ?? 0;
    const [from, to] = rangeForViewport(0, lay.total, g, n, (lay.top[si] ?? 0) + 44, 0);
    assert.equal(to - from, n, `all of section ${si} in a viewport over the whole document`);
  }
  // down from the last row of a month lands in the next shown month, same column (folded "b" is skipped)
  const lastOfA = (ix.effStart[0] ?? 0) + (ix.effCount[0] ?? 1) - 1;
  const next = gridMove(lastOfA, "ArrowDown", ix.effCount, g.cols);
  assert.equal(sectionOfEff(ix, next), 3, "folded b and empty c are skipped");
});

test("RowCache: pages by absolute index, missing pages, eviction keeps every wanted span and page 0", () => {
  const c = new RowCache<number>(4, 3);
  assert.equal(c.get(5), undefined);
  c.put(1, [4, 5, 6, 7]);
  assert.equal(c.get(5), 5);
  assert.deepEqual(c.missing(2, 10), [0, 2]);
  assert.deepEqual(c.missing(4, 8), []);
  for (const p of [0, 2, 3, 9, 10]) c.put(p, [p, p, p, p]);
  assert.equal(c.size, 6);
  // viewport on page 9 (rows 36..39), the selected row far away on page 1 (rows 4..5)
  const dropped = c.trim([36, 40, 4, 6]);
  assert.equal(c.size, 3);
  assert.ok(c.has(0) && c.has(9) && c.has(1), "page 0, the viewport and the selection's page stay");
  assert.ok(!dropped.includes(0) && !dropped.includes(9) && !dropped.includes(1));
  assert.deepEqual(c.trim([36, 40]), [], "under the cap nothing goes");
  assert.equal(PAGE, 256);
});
