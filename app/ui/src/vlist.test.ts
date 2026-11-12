import assert from "node:assert/strict";
import { test } from "node:test";
import { VLIST_MAX_PX, vlist } from "./vlist.ts";

const ROW = 54;

test("small lists behave like the plain virtual scroller", () => {
  const g = vlist(1000, ROW, 5400, 700, 6); // row 100 at the top of the viewport
  assert.equal(g.height, 54_000);
  assert.equal(g.voff, 5400);
  assert.equal(g.first, 100 - 6);
  assert.equal(g.last, Math.ceil((5400 + 700) / ROW) + 6);
  // zero results: no rows, empty spacer
  const empty = vlist(0, ROW, 0, 700, 6);
  assert.equal(empty.height, 0);
  assert.equal(empty.first, 0);
  assert.equal(empty.last, -1);
});

test("windows tile the list and stay inside it", () => {
  const total = 10_000;
  const seen = new Set<number>();
  for (let st = 0; st <= total * ROW - 700; st += 350) {
    const { first, last } = vlist(total, ROW, st, 700, 0);
    for (let i = first; i <= last; i++) seen.add(i);
    assert.ok(first >= 0 && last < total, `${first}..${last} out of range at ${st}`);
  }
  // the scrollbar's exact bottom position (the step above can skip it)
  const pinned = vlist(total, ROW, total * ROW - 700, 700, 0);
  for (let i = pinned.first; i <= pinned.last; i++) seen.add(i);
  assert.equal(seen.size, total, "every row must be visible at some scroll position");
});

test("huge lists clamp the spacer and map the scroll proportionally", () => {
  const total = 1_000_000; // 54M px > the 30M cap
  const g = vlist(total, ROW, 0, 800, 6);
  assert.equal(g.height, VLIST_MAX_PX, "spacer is capped so the DOM survives");
  assert.equal(g.voff, 0);
  assert.ok(g.first <= 6 + 1 && g.last >= 14, "row 0 is visible at the top");

  const mid = vlist(total, ROW, (VLIST_MAX_PX - 800) / 2, 800, 6);
  assert.ok(mid.voff > (total * ROW - 800) / 2 - ROW && mid.voff < (total * ROW - 800) / 2 + ROW, "mid scroll maps to mid list");
  assert.ok(Math.abs((mid.first + mid.last) / 2 - total / 2) < 20, "mid scroll shows the middle rows");

  const end = vlist(total, ROW, VLIST_MAX_PX - 800, 800, 6); // scrollbar pinned at the bottom
  assert.equal(end.last, total - 1, "the last row is reachable");
  assert.ok(end.first > total - 40, "the bottom window hugs the end of the list");
  // the property that matters at 1M+ rows: every visible row renders NEAR the viewport, so
  // its translateY stays tiny and can never hit Chromium's transform clamp
  for (const g of [mid, end]) {
    for (let i = g.first; i <= g.last; i++) {
      const y = i * ROW - g.voff;
      assert.ok(y > -(6 + 1) * ROW && y < 800 + (6 + 1) * ROW, `row ${i} would render at y=${y}, off-screen`);
    }
  }
});

test("clamped scroll input never escapes the list", () => {
  const g = vlist(100, ROW, 999_999, 700, 6); // scrolled past the end
  assert.ok(g.first >= 0 && g.last <= 99);
  assert.equal(g.last, 99, "pinned at the last row");
});
