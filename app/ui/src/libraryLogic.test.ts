import { test } from "node:test";
import assert from "node:assert/strict";
import {
  activeSection, freedBytes, jumpOffset, scrubSlot, sectionOfKey, stepMonth, type RailSlot, gridCols, groupByMonth, inBatch, isScreenshot, layoutGrid, moveIndex, pickKeeper, railEntries,
  rangedBatch, rowOfItem, scrollIntoView, toggledBatch, totalReclaimable,
  uniformRange, visibleRows,
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
  assert.equal(L.rows[0]?.top, 8);
  assert.equal(L.rows[1]?.top, 48);
  assert.equal(L.rows[2]?.top, 158);
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

test("timeline views: app graphics only in their own view", async () => {
  const { countViews, inView, parseTimelineView } = await import("./libraryLogic.ts");
  const items = [{ class: "photo" }, { class: "photo" }, { class: "screenshot" }, { class: "graphic" }, { class: "graphic" }, { class: "graphic" }] as const;
  assert.deepEqual(countViews([...items]), { all: 3, photos: 2, screenshots: 1, graphics: 3 });
  assert.equal(inView("graphic", "all"), false);
  assert.equal(inView("screenshot", "all"), true);
  assert.equal(inView("screenshot", "photos"), false);
  assert.equal(inView("graphic", "graphics"), true);
  assert.equal(parseTimelineView("screenshots"), "screenshots");
  assert.equal(parseTimelineView("nonsense"), "all");
  assert.equal(parseTimelineView(null), "all");
});

test("collection ideas: several phrasings each, taken names drop out", async () => {
  const { COLLECTION_IDEAS, freshIdeas } = await import("./libraryLogic.ts");
  for (const i of COLLECTION_IDEAS) {
    const parts = i.query.split(",").map((p) => p.trim());
    assert.ok(parts.length >= 2 && parts.length <= 5, `${i.name}: 2..5 phrasings (the backend searches at most 6 incl. the whole text)`);
    assert.ok(parts.every((p) => p.split(/\s+/).length >= 2), `${i.name}: one-word parts are not searched alone`);
  }
  assert.equal(freshIdeas(["medical ", "FOOD"]).some((i) => i.name === "Medical" || i.name === "Food"), false);
  assert.equal(freshIdeas([]).length, COLLECTION_IDEAS.length);
});

test("oldScreenshots: only screenshots past the age, biggest first", async () => {
  const { oldScreenshots } = await import("./libraryLogic.ts");
  const day = 24 * 3600 * 1000, now = 1000 * day;
  const items = [
    { id: "new", class: "screenshot" as const, mtime: now - 10 * day, size: 9 },
    { id: "old-small", class: "screenshot" as const, mtime: now - 200 * day, size: 1 },
    { id: "old-big", class: "screenshot" as const, mtime: now - 400 * day, size: 5 },
    { id: "photo", class: "photo" as const, mtime: now - 900 * day, size: 99 },
  ];
  assert.deepEqual(oldScreenshots(items, now).map((i) => i.id), ["old-big", "old-small"]);
  assert.deepEqual(oldScreenshots(items, now, 5).map((i) => i.id), ["new", "old-big", "old-small"]);
});

test("parseGalleryView: only 'list' is a list, everything else is the grid", async () => {
  const { parseGalleryView } = await import("./libraryLogic.ts");
  assert.equal(parseGalleryView("list"), "list");
  assert.equal(parseGalleryView("grid"), "grid");
  assert.equal(parseGalleryView("timeline"), "grid");
  assert.equal(parseGalleryView(null), "grid");
  assert.equal(parseGalleryView(undefined), "grid");
  assert.equal(parseGalleryView(42), "grid");
});

test("nameMatches: case-insensitive substring, empty query matches all", async () => {
  const { nameMatches } = await import("./libraryLogic.ts");
  assert.equal(nameMatches("PXL_20261112_164901.jpg", "pxl"), true);
  assert.equal(nameMatches("PXL_20261112_164901.jpg", "  PX_2 "), false);
  assert.equal(nameMatches("invoice march.pdf", "March"), true);
  assert.equal(nameMatches("anything.txt", "   "), true);
  assert.equal(nameMatches("a.wav", "a"), true);
});

test("parseFoldSet: a JSON array of strings becomes the fold set, junk becomes empty", async () => {
  const { parseFoldSet } = await import("./libraryLogic.ts");
  assert.deepEqual([...parseFoldSet(["2026-03", "2025-11"])].sort(), ["2025-11", "2026-03"]);
  assert.equal(parseFoldSet([]).size, 0);
  assert.equal(parseFoldSet("2026-03").size, 0);
  assert.equal(parseFoldSet([1, "2026-03"]).size, 1);
  assert.equal(parseFoldSet(null).size, 0);
});

test("toggledBatch: adds, removes case-insensitively, keeps real case, may empty", () => {
  assert.deepEqual(toggledBatch(new Set(), "C:\\a"), ["C:\\a"]);
  assert.deepEqual(toggledBatch(new Set(["C:\\a"]), "C:\\B"), ["C:\\a", "C:\\B"]);
  assert.deepEqual(toggledBatch(new Set(["C:\\a", "C:\\B"]), "c:\\A"), ["C:\\B"]);
  assert.deepEqual(toggledBatch(new Set(["C:\\a"]), "C:\\A"), []); // toggled off: empty, caller falls back to the lead
});

test("rangedBatch: the shown order between anchor and click, anchor lost means just the click", () => {
  const order = ["C:\\a", "C:\\b", "C:\\c", "C:\\d"];
  assert.deepEqual(rangedBatch(order, "C:\\b", "C:\\d"), ["C:\\b", "C:\\c", "C:\\d"]);
  assert.deepEqual(rangedBatch(order, "C:\\d", "C:\\b"), ["C:\\b", "C:\\c", "C:\\d"]); // backwards works too
  assert.deepEqual(rangedBatch(order, "C:\\b", "C:\\B"), ["C:\\B"]); // the anchor row itself: just the click
  assert.deepEqual(rangedBatch(order, "C:\\gone", "C:\\c"), ["C:\\c"]); // anchor left the surface
  assert.deepEqual(rangedBatch(order, null, "C:\\c"), ["C:\\c"]); // no anchor yet
});

test("inBatch: batch membership and single-lead fallback, case-insensitive", () => {
  assert.equal(inBatch(new Set(["C:\\a"]), "c:\\A"), true);
  assert.equal(inBatch(null, "C:\\a", "C:\\a"), true);
  assert.equal(inBatch(null, "C:\\a", "c:\\b"), false);
  assert.equal(inBatch(null, "C:\\a"), false);
});

test("activeSection matches a linear scan on a big timeline (binary search)", () => {
  const tops = Array.from({ length: 20000 }, (_, i) => i * 300 + 8);
  for (const st of [0, 4, 5, 299, 307, 308, 123456, 5_999_000, 99_999_999]) {
    let want = 0;
    tops.forEach((t, i) => { if (t <= st + 4) want = i; });
    assert.equal(activeSection(tops, st), want);
  }
});

test("jumpOffset / sectionOfKey: a month key lands on its section top", () => {
  const secs = [{ key: "2026-11" }, { key: "2026-10" }, { key: "2025-12" }];
  const tops = [8, 400, 1500];
  assert.equal(jumpOffset(tops, sectionOfKey(secs, "2026-10")), 400);
  assert.equal(jumpOffset(tops, 99), 1500);
  assert.equal(jumpOffset(tops, -3), 8);
  assert.equal(jumpOffset([], 2), 0);
  assert.equal(sectionOfKey(secs, "1999-01"), -1);
});

test("railEntries keeps the real section index of filtered entries", () => {
  const r = railEntries([{ year: 2026, month: 0, count: 2, idx: 3 }, { year: 2026, month: 1, count: 0, idx: 5 }]);
  assert.deepEqual(r[0]!.months.map((m) => m.section), [3, 5]);
});

const slots: RailSlot[] = [
  { top: 20, bottom: 42, count: 5 },   // 0
  { top: 42, bottom: 64, count: 0 },   // 1 empty
  { top: 64, bottom: 86, count: 0 },   // 2 empty
  { top: 110, bottom: 132, count: 7 }, // 3 (after a year heading gap)
];
test("scrubSlot: y to month, clamped, gaps, empty months skipped", () => {
  assert.equal(scrubSlot(slots, 25), 0);
  assert.equal(scrubSlot(slots, -50), 0);
  assert.equal(scrubSlot(slots, 9999), 3);
  assert.equal(scrubSlot(slots, 120), 3);
  assert.equal(scrubSlot(slots, 100), 3); // the heading gap goes to the nearer month
  assert.equal(scrubSlot(slots, 90, 0), 3); // moving down skips the empty ones forward
  assert.equal(scrubSlot(slots, 50, 3), 0); // moving up skips them backward
  assert.equal(scrubSlot([{ top: 0, bottom: 10, count: 0 }], 5), -1);
  assert.equal(scrubSlot([], 5), -1);
});

test("stepMonth skips empty months and stops at the ends", () => {
  const c = [4, 0, 0, 9, 1];
  assert.equal(stepMonth(c, 0, 1), 3);
  assert.equal(stepMonth(c, 3, -1), 0);
  assert.equal(stepMonth(c, 4, 1), 4);
  assert.equal(stepMonth(c, 0, -1), 0);
});
