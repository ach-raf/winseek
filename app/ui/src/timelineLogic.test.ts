import assert from "node:assert/strict";
import { test } from "node:test";
import { mosaicMetrics, moveMosaic, packMosaic, tileBox } from "./libraryLogic.ts";
import {
  PAGE, RowCache, absToEff, buildIndex, effRangeToAbs, effToAbs, moveBy, packTemplate, rangeAdd, rangeCount, rangeHas, rangeRemoveIndex, rangeToggle,
  sectionOfAbs, sectionOfEff, templateBox, tilesInBand, type Range,
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

test("template: a month of any size is the first tiles of the same packing (equals packMosaic)", () => {
  for (const cols of [4, 6, 8]) {
    const t = packTemplate(200, cols);
    for (const n of [0, 1, 5, 13, 64, 200]) {
      const m = packMosaic([n], cols);
      assert.equal(t.rowsAt[n], m.sections[0]!.rows, `rows n=${n} cols=${cols}`);
      for (let j = 0; j < n; j++) {
        const a = m.tiles[j]!;
        assert.deepEqual([t.col[j], t.row[j], t.w[j], t.h[j]], [a.col, a.row, a.w, a.h]);
      }
    }
  }
});

test("template: boxes match the old tile boxes", () => {
  const t = packTemplate(30, 8);
  const m = packMosaic([30], 8);
  for (let j = 0; j < 30; j++) {
    const b = templateBox(t, j, 100, 4, 44);
    const o = tileBox(m.tiles[j]!, 100, 4, 44);
    assert.deepEqual([b.y, b.h], [o.y, o.h]);
  }
});

test("tilesInBand never misses a tile that intersects the band", () => {
  const rowH = 100, gap = 4, head = 44;
  for (const cols of [4, 8]) {
    const n = 300;
    const t = packTemplate(n, cols);
    for (const [y0, y1] of [[0, 300], [500, 900], [1500, 1700], [-100, 60], [4000, 9000]] as const) {
      const [from, to] = tilesInBand(t, n, rowH, gap, head, y0, y1);
      for (let j = 0; j < n; j++) {
        const b = templateBox(t, j, rowH, gap, head);
        if (b.y + b.h >= y0 && b.y <= y1) assert.ok(j >= from && j < to, `tile ${j} in [${y0},${y1}] cols=${cols}`);
      }
      assert.ok(to - from < n || y1 > 4000, "a narrow band does not return the whole month");
    }
  }
});

test("moveBy walks like moveMosaic inside one month", () => {
  const n = 40;
  const m = packMosaic([n], 8);
  const mt = mosaicMetrics(m, 100, 4, [44], 14, 20);
  const t = packTemplate(n, 8);
  const boxAt = (k: number) => templateBox(t, k, 100, 4, 44);
  for (const key of ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"]) {
    for (const i of [0, 1, 7, 12, 25, 39]) {
      assert.equal(moveBy(i, key, n, boxAt), moveMosaic(m, mt, 100, 4, i, key, n), `${key} from ${i}`);
    }
  }
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
