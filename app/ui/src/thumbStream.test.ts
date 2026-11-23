import assert from "node:assert/strict";
import { test } from "node:test";
import { coverRect, needOf, wantOrder } from "./thumbStream.ts";
import { mockThumbStream } from "./mockThumbs.ts";
import type { ThumbBatch, ThumbWant } from "./types.ts";

test("wantOrder: on screen first, then nearest the viewport centre", () => {
  const xs = [
    { id: "ring-near", vis: false, dist: 10 },
    { id: "vis-far", vis: true, dist: 500 },
    { id: "vis-near", vis: true, dist: 20 },
    { id: "ring-far", vis: false, dist: 900 },
  ];
  assert.deepEqual(xs.sort(wantOrder).map((x) => x.id), ["vis-near", "vis-far", "ring-near", "ring-far"]);
});

test("needOf: a fling asks the instant level only; held or failed levels are not asked again", () => {
  assert.equal(needOf(0, false), 3);
  assert.equal(needOf(0, true), 1);
  assert.equal(needOf(1, false), 2); // instant level shown: only the decode is left
  assert.equal(needOf(1, true), 0);
  assert.equal(needOf(4, false), 2); // no instant level on disk: straight to the decode
  assert.equal(needOf(2, false), 0); // decoded picture held
  assert.equal(needOf(8, false), 1); // decode failed: the instant level may still show something
});

test("coverRect: crops the centre so the picture covers the box", () => {
  assert.deepEqual(coverRect(400, 300, 200, 150), [0, 0, 400, 300]);
  assert.deepEqual(coverRect(300, 400, 200, 150), [0, 87.5, 300, 225]);
  assert.deepEqual(coverRect(800, 300, 200, 150), [200, 0, 400, 300]);
});

test("mock stream: a new wanted list replaces the old one and only wanted tiles are delivered", async () => {
  const s = mockThumbStream();
  const got: ThumbBatch[] = [];
  const g = await s.thumbsOpen((b) => got.push(b));
  const w = (k: string, vis: 0 | 1): ThumbWant => [k, `C:/p/${k}.jpg`, 40, 30, 3, vis];
  await s.thumbsWant(g, Array.from({ length: 40 }, (_, i) => w(`old${i}`, 1)));
  // The scroll moved on before most of it was read.
  await s.thumbsWant(g, [w("new", 1)]);
  await new Promise((r) => setTimeout(r, 150));
  const keys = new Set(got.flatMap((b) => b.items.map((i) => i.k)));
  assert.ok(keys.has("new"));
  const old = [...keys].filter((k) => k.startsWith("old")).length;
  assert.ok(old <= 4, `at most the reads already under way finish for old tiles, got ${old}`);
  const levels = got.flatMap((b) => b.items.filter((i) => i.k === "new").map((i) => i.l));
  assert.ok(levels.includes(2), "the decoded level arrives");
});
