import { test } from "node:test";
import assert from "node:assert/strict";
import {
  activeSection, freedBytes, gridCols, groupByMonth, isScreenshot, layoutGrid, moveIndex, pickKeeper, railEntries,
  rowOfItem, scrollIntoView, totalReclaimable, uniformRange, visibleRows,
} from "./libraryLogic.ts";

const at = (y: number, m: number, d: number) => new Date(y, m - 1, d, 12).getTime();

test("groupByMonth: newest month first, newest item first, rail follows", () => {
  const items = [
    { id: "a", mtime: at(2024, 12, 31) },
    { id: "b", mtime: at(2025, 1, 2) },
    { id: "c", mtime: at(2025, 1, 20) },
    { id: "d", mtime: at(2023, 6, 1) },
  ];
  const s = groupByMonth(items);
  assert.deepEqual(s.map((x) => [x.key, x.label, x.items.map((i) => i.id)]), [
    ["2025-01", "January 2025", ["c", "b"]],
    ["2024-12", "December 2024", ["a"]],
    ["2023-06", "June 2023", ["d"]],
  ]);
  assert.deepEqual(railEntries(s), [
    { year: 2025, months: [{ section: 0, label: "Jan", count: 2 }] },
    { year: 2024, months: [{ section: 1, label: "Dec", count: 1 }] },
    { year: 2023, months: [{ section: 2, label: "Jun", count: 1 }] },
  ]);
  assert.deepEqual(groupByMonth([]), []);
});

test("gridCols fills the width with square cells", () => {
  assert.deepEqual(gridCols(1000, 160, 8), { cols: 5, cell: 185 });
  const g = gridCols(300, 160, 8);
  assert.equal(g.cols, 1);
  assert.ok(g.cell >= 32);
  assert.equal(gridCols(0, 160, 8).cols, 1);
});

test("layoutGrid: headings, wrapped rows, section offsets", () => {
  const L = layoutGrid([5, 0, 2], ["A", "B", "C"], 3, 100, 10, 40);
  assert.deepEqual(L.rows.map((r) => [r.kind, r.section, r.start, r.end]), [
    ["head", 0, 0, 0], ["cells", 0, 0, 3], ["cells", 0, 3, 5], ["head", 2, 5, 5], ["cells", 2, 5, 7],
  ]);
  assert.equal(L.rows[0].top, 8);
  assert.equal(L.rows[1].top, 48);
  assert.equal(L.rows[2].top, 158);
  assert.equal(L.sectionTop[0], 8);
  assert.equal(L.sectionTop[2], 268);
  assert.equal(L.total, 268 + 40 + 110 + 20);
  assert.equal(rowOfItem(L, 4)?.top, 158);
  assert.equal(rowOfItem(L, 6)?.section, 2);
  assert.equal(rowOfItem(L, 7), null);
  // Without labels there are no heading rows.
  assert.deepEqual(layoutGrid([4], [null], 2, 50, 0, 40).rows.map((r) => r.kind), ["cells", "cells"]);
});

test("visibleRows windows by pixel range with overscan", () => {
  const rows = Array.from({ length: 1000 }, (_, i) => ({ top: i * 100, h: 100 }));
  assert.deepEqual(visibleRows(rows, 0, 300, 0), [0, 3]);
  assert.deepEqual(visibleRows(rows, 5000, 250, 0), [50, 53]);
  assert.deepEqual(visibleRows(rows, 5000, 250, 100), [49, 54]);
  assert.deepEqual(visibleRows(rows, 999999, 100, 0), [1000, 1000]);
  assert.deepEqual(visibleRows([], 0, 100, 0), [0, 0]);
});

test("uniformRange", () => {
  assert.deepEqual(uniformRange(100, 50, 0, 200, 2), [0, 6]);
  assert.deepEqual(uniformRange(100, 50, 1000, 200, 2), [18, 26]);
  assert.deepEqual(uniformRange(3, 50, 0, 400, 2), [0, 3]);
  assert.deepEqual(uniformRange(0, 50, 0, 400, 2), [0, 0]);
});

test("scrollIntoView and moveIndex", () => {
  const row = { top: 500, h: 100 };
  assert.equal(scrollIntoView(row, 450, 300), 450, "already visible");
  assert.equal(scrollIntoView(row, 300, 250), 358, "below the fold: bottom edge + margin");
  assert.equal(scrollIntoView(row, 0, 1000), 0);
  assert.equal(scrollIntoView(row, 800, 300), 492);
  assert.equal(moveIndex(0, "ArrowLeft", 4, 10), 0);
  assert.equal(moveIndex(9, "ArrowRight", 4, 10), 9);
  assert.equal(moveIndex(5, "ArrowUp", 4, 10), 1);
  assert.equal(moveIndex(1, "ArrowUp", 4, 10), 1);
  assert.equal(moveIndex(5, "ArrowDown", 4, 10), 9);
  assert.equal(moveIndex(7, "ArrowDown", 4, 10), 9, "short last row: jump to the last item");
  assert.equal(moveIndex(9, "ArrowDown", 4, 10), 9);
  assert.equal(moveIndex(3, "End", 4, 10), 9);
  assert.equal(moveIndex(3, "Home", 4, 10), 0);
  assert.equal(moveIndex(3, "x", 4, 0), 0);
});

test("activeSection follows the scroll position", () => {
  assert.equal(activeSection([8, 300, 900], 0), 0);
  assert.equal(activeSection([8, 300, 900], 298), 1);
  assert.equal(activeSection([8, 300, 900], 5000), 2);
  assert.equal(activeSection([], 10), 0);
});

test("isScreenshot", () => {
  for (const n of ["Screenshot (88).png", "Screenshot 2025-03-14 101112.png", "screen shot 1.png", "Screen Capture.png", "Snip_2.png", "Capture 12.png"]) assert.ok(isScreenshot(n), n);
  for (const n of ["IMG_0412.jpg", "holiday.png", "capture-card-manual.jpg", "Receipt.jpg"]) assert.ok(!isScreenshot(n), n);
});

test("pickKeeper: resolution, then newest, then shortest path", () => {
  const m = (path: string, mtime: number, width: number | null, height: number | null) => ({ path, mtime, width, height });
  assert.equal(pickKeeper([m("a.jpg", 5, 100, 100), m("b.jpg", 1, 200, 200), m("c.jpg", 9, 150, 150)]), 1, "most pixels wins over newer");
  assert.equal(pickKeeper([m("a.jpg", 5, 100, 100), m("b.jpg", 9, 100, 100)]), 1, "equal pixels: newest");
  assert.equal(pickKeeper([m("C:\\long\\name copy.jpg", 5, null, null), m("C:\\a.jpg", 5, null, null)]), 1, "equal date: shortest path");
  assert.equal(pickKeeper([m("b.jpg", 5, 1, 1), m("a.jpg", 5, 1, 1)]), 1, "full tie: alphabetical first");
  assert.equal(pickKeeper([m("x", 1, null, null)]), 0);
});

test("freed bytes and totals", () => {
  assert.equal(freedBytes([{ size: 10 }, { size: 20 }, { size: 5 }], 1), 15);
  assert.equal(totalReclaimable([{ reclaimable: 3 }, { reclaimable: 4 }]), 7);
});
