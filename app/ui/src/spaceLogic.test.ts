import { test } from "node:test";
import assert from "node:assert/strict";
import { crumbs, formatPct, isHidden, parentPath, squarify, tileItems, withoutHidden } from "./spaceLogic.ts";
import type { DiskUsage, UsageEntry } from "./types.ts";

const ent = (name: string, size: number, dir = false): UsageEntry =>
  ({ name, path: `C:\\x\\${name}`, is_dir: dir, kind: 1, size, files: dir ? 3 : 1, folders: 0, mtime: null });
const usage = (entries: UsageEntry[], more = 0, more_size = 0): DiskUsage =>
  ({ path: "C:\\x", size: entries.reduce((a, e) => a + e.size, 0) + more_size, files: 9, folders: 2, entries, more, more_size, types: [], biggest: [], drives: [] });

test("squarify tiles the box exactly and keeps areas proportional", () => {
  const box = { x: 0, y: 0, w: 400, h: 300 };
  const t = squarify([6, 6, 4, 3, 2, 2, 1].map((v, i) => ({ item: i, value: v })), box);
  assert.equal(t.length, 7);
  const area = t.reduce((a, r) => a + r.w * r.h, 0);
  assert.ok(Math.abs(area - 120000) < 0.01);
  for (const r of t) {
    assert.ok(r.x >= -1e-6 && r.y >= -1e-6 && r.x + r.w <= 400 + 1e-6 && r.y + r.h <= 300 + 1e-6);
    assert.ok(Math.abs(r.w * r.h - (r.value / 24) * 120000) < 0.01);
  }
});

test("squarify handles empty input and one item", () => {
  assert.deepEqual(squarify([], { x: 0, y: 0, w: 10, h: 10 }), []);
  const [only] = squarify([{ item: "a", value: 5 }], { x: 0, y: 0, w: 10, h: 20 });
  assert.deepEqual([only.x, only.y, only.w, only.h], [0, 0, 10, 20]);
});

test("crumbs and parentPath walk drive -> folder", () => {
  assert.deepEqual(crumbs("").map((c) => c.path), [""]);
  assert.deepEqual(crumbs("C:\\Users\\Me").map((c) => c.path), ["", "C:\\", "C:\\Users", "C:\\Users\\Me"]);
  assert.equal(parentPath("C:\\Users\\Me"), "C:\\Users");
  assert.equal(parentPath("C:\\Users"), "C:\\");
  assert.equal(parentPath("C:\\"), "");
  assert.equal(parentPath(""), "");
});

test("withoutHidden removes recycled entries and their bytes, also folders' contents", () => {
  const d = usage([ent("a.mp4", 700), ent("Pics", 200, true), ent("b.txt", 100)]);
  const hidden = new Set(["c:\\X\\Pics", "C:\\x\\a.mp4"]);
  assert.ok(isHidden("C:\\x\\Pics\\deep\\f.jpg", hidden));
  assert.ok(!isHidden("C:\\x\\Pics2", hidden));
  const r = withoutHidden(d, hidden);
  assert.deepEqual(r.entries.map((e) => e.name), ["b.txt"]);
  assert.equal(r.size, 100);
  assert.equal(withoutHidden(d, new Set()), d);
});

test("tileItems folds tiny entries and the service's leftovers into one rest tile", () => {
  const d = usage([ent("big", 9000), ent("tiny1", 1), ent("tiny2", 1), ent("zero", 0)], 5, 10);
  const t = tileItems(d, { x: 0, y: 0, w: 300, h: 200 });
  assert.equal(t.length, 2);
  const rest = t[1].item as { rest: true; count: number; size: number };
  assert.equal(rest.rest, true);
  assert.deepEqual([rest.count, rest.size], [7, 12]);
  assert.deepEqual(tileItems(usage([]), { x: 0, y: 0, w: 10, h: 10 }), []);
});

test("formatPct", () => {
  assert.equal(formatPct(42.4), "42%");
  assert.equal(formatPct(3.14), "3.1%");
  assert.equal(formatPct(0.04), "<0.1%");
  assert.equal(formatPct(0), "0%");
});
