import assert from "node:assert/strict";
import { test } from "node:test";
import { MIN_COLS, PIC_GAP, PIC_H, PIC_W, gridMetrics, gridMove, gridStep, quickLookKey, rangeForViewport, rowOf, rowsFor, sectionLayout } from "./gridLogic.ts";

test("gridMetrics: the tile size is the same at every width; only the column count changes", () => {
  for (const w of [560, 600, 748, 900, 1200]) {
    const g = gridMetrics(w);
    assert.equal(g.tileW, PIC_W, `width ${w}`);
    assert.equal(g.tileH, PIC_H);
    assert.equal(g.rowStride, PIC_H + PIC_GAP);
    assert.ok(g.width <= w, `fits ${w}`);
    assert.ok(g.width + PIC_W + PIC_GAP > w, `no room for one more at ${w}`);
  }
  assert.equal(gridMetrics(748).cols, 4); // the compact popup
  assert.equal(gridMetrics(602).cols, 3); // the library next to its preview dock
  assert.equal(gridMetrics(748, 4).cols, 4);
  assert.equal(gridMetrics(2000, 5).cols, 5);
});

test("gridMetrics: below three columns the tiles shrink, same aspect", () => {
  const g = gridMetrics(300);
  assert.equal(g.cols, MIN_COLS);
  assert.ok(g.tileW < PIC_W);
  assert.ok(g.width <= 300);
  assert.equal(g.tileH, Math.round((g.tileW * 3) / 4));
});

test("rowOf / rowsFor / rangeForViewport are arithmetic", () => {
  assert.equal(rowOf(0, 4), 0);
  assert.equal(rowOf(7, 4), 1);
  assert.equal(rowsFor(0, 4), 0);
  assert.equal(rowsFor(9, 4), 3);
  const g = { cols: 4, rowStride: 140 };
  assert.deepEqual(rangeForViewport(0, 280, g, 100, 0, 0), [0, 8]);
  assert.deepEqual(rangeForViewport(1400, 280, g, 100, 0, 1), [36, 52]);
  assert.deepEqual(rangeForViewport(99999, 280, g, 100, 0, 1), [0, 0]);
  // never more than the window: a 1,700 item grid draws a few rows
  const [a, b] = rangeForViewport(50_000, 720, { cols: 3, rowStride: 140 }, 1700, 44, 2);
  assert.ok(b - a <= 3 * 10, `bounded: ${b - a}`);
});

test("sectionLayout stacks headings and whole rows", () => {
  const g = { cols: 3, rowStride: 140, gap: 8 };
  const l = sectionLayout([4, 0, 3], [44, 44, 6], g, 14);
  assert.deepEqual(l.top, [0, 44 + 2 * 140 - 8 + 14, 44 + 2 * 140 - 8 + 14 + 44 + 6]);
  assert.equal(l.h[2], 6 + 140 - 8 + 14);
  assert.equal(l.total, l.top[2]! + l.h[2]!);
});

test("gridStep: rows stay rows, edges leave the grid", () => {
  // 10 tiles in 4 columns: rows 0-3, 4-7, 8-9
  assert.equal(gridStep(0, 10, 4, "left"), 0);
  assert.equal(gridStep(3, 10, 4, "right"), 3);
  assert.equal(gridStep(1, 10, 4, "down"), 5);
  assert.equal(gridStep(6, 10, 4, "down"), 9, "a shorter row below: its last tile");
  assert.equal(gridStep(9, 10, 4, "down"), 10, "leaves below");
  assert.equal(gridStep(2, 10, 4, "up"), -1, "leaves above");
  assert.equal(gridStep(9, 10, 4, "right"), 9);
});

test("gridMove crosses months in the same column", () => {
  const counts = [6, 0, 5]; // 3 columns: [0-2][3-5] | (empty) | [6-8][9-10]
  assert.equal(gridMove(4, "ArrowDown", counts, 3), 7);
  assert.equal(gridMove(5, "ArrowDown", counts, 3), 8);
  assert.equal(gridMove(7, "ArrowUp", counts, 3), 4);
  assert.equal(gridMove(10, "ArrowDown", counts, 3), 10, "bottom stays");
  assert.equal(gridMove(1, "ArrowUp", counts, 3), 1, "top stays");
  assert.equal(gridMove(9, "ArrowRight", counts, 3), 10);
  assert.equal(gridMove(10, "ArrowRight", counts, 3), 10);
  assert.equal(gridMove(3, "Home", counts, 3), 0);
  assert.equal(gridMove(3, "End", counts, 3), 10);
  assert.equal(gridMove(0, "PageDown", counts, 3, 3), 9);
  assert.equal(gridMove(9, "PageUp", counts, 3, 3), 0);
  assert.equal(gridMove(0, "x", counts, 3), 0);
  assert.equal(gridMove(5, "ArrowDown", [], 3), 0);
});

test("quickLookKey: Space toggles, Escape closes only while open", () => {
  assert.equal(quickLookKey(" ", false), "toggle");
  assert.equal(quickLookKey(" ", true), "toggle");
  assert.equal(quickLookKey("Escape", true), "close");
  assert.equal(quickLookKey("Escape", false), null);
  assert.equal(quickLookKey("Enter", true), null);
});
