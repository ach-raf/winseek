import assert from "node:assert/strict";
import { test } from "node:test";
import { VLIST_MAX_PX, pagesToEvict, scrollTopFor, vlist, vlistClipped } from "./vlist.ts";

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

test("a pinned offset is exact on huge lists and ignored on small ones", () => {
  const total = 9_000_000; // 486M px: one scrollbar pixel is ~16 list pixels
  assert.ok(vlistClipped(total, ROW));
  const row = 4_512_340;
  const g = vlist(total, ROW, 123, 800, 0, row * ROW);
  assert.equal(g.voff, row * ROW, "the pinned row sits exactly at the top");
  assert.equal(g.first, row);
  // the thumb position for that offset maps back to (about) the same place
  const st = scrollTopFor(total, ROW, 800, row * ROW);
  assert.ok(st > 0 && st < VLIST_MAX_PX - 800);
  const back = vlist(total, ROW, st, 800, 0);
  assert.ok(Math.abs(back.voff - row * ROW) < 1, "inverse mapping round-trips");
  // pins clamp to the list
  assert.equal(vlist(total, ROW, 0, 800, 0, -50).voff, 0);
  assert.equal(vlist(total, ROW, 0, 800, 0, 1e12).voff, total * ROW - 800);
  // small lists: the scrollbar is exact, a pin changes nothing
  assert.ok(!vlistClipped(1000, ROW));
  assert.equal(vlist(1000, ROW, 5400, 700, 0, 999).voff, 5400);
  assert.equal(scrollTopFor(1000, ROW, 700, 5400), 5400);
});

test("page eviction keeps the visible pages, page 0 and the nearest neighbours", () => {
  const loaded = [0, 1, 2, 500, 501, 502, 503, 9000];
  assert.deepEqual(pagesToEvict(loaded, 501, 502, 20), [], "under the cap nothing goes");
  const drop = pagesToEvict(loaded, 501, 502, 5);
  assert.deepEqual(drop, [9000, 1, 2], "farthest first; 0 and the visible pages are never dropped");
  assert.ok(!drop.includes(0) && !drop.includes(501) && !drop.includes(502));
});

test("rows land under the viewport at every scroll position (never left behind at the top)", () => {
  const viewH = 700;
  for (const total of [50, 10_000, 1_000_000, 9_000_000]) {
    const max = Math.max(0, Math.min(total * ROW, VLIST_MAX_PX) - viewH);
    for (const st of [0, 54, 5400, max / 2, max - 1, max]) {
      const g = vlist(total, ROW, st, viewH, 0);
      for (let i = g.first; i <= g.last; i++) {
        const y = g.top + i * ROW - g.voff; // position inside the spacer
        const onScreen = y - Math.min(st, max); // position inside the viewport
        assert.ok(onScreen > -2 * ROW && onScreen < viewH + ROW, `total ${total} st ${st} row ${i}: ${onScreen}px`);
        assert.ok(y > -2 * ROW && y < g.height + ROW, "inside the spacer");
      }
      // the viewport is covered from top to bottom
      assert.ok(g.top + g.first * ROW - g.voff <= Math.min(st, max) + 0.001, "top edge covered");
      if (total * ROW > viewH) assert.ok(g.top + (g.last + 1) * ROW - g.voff >= Math.min(st, max) + viewH - 0.001, "bottom edge covered");
    }
  }
  // a pinned (wheel / keyboard) offset keeps the rows under the real scrollbar position too
  const g = vlist(9_000_000, ROW, 15_000_000, viewH, 0, 4_000_000 * ROW);
  assert.equal(g.top + 4_000_000 * ROW - g.voff, 15_000_000);
});
