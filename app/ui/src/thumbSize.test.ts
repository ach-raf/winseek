import assert from "node:assert/strict";
import { test } from "node:test";
import { LOW_PX, thumbPair, thumbPx, thumbUrl } from "./thumbSize.ts";

test("snaps up to a bucket and never below the cover size", () => {
  assert.equal(thumbPx(104, 104, 1), 192); // 104 * 1.4 = 146 -> 192
  assert.equal(thumbPx(104, 104, 1.25), 192); // 182 -> 192
  assert.equal(thumbPx(132, 132, 1.25), 256); // 231 -> 256
  assert.equal(thumbPx(212, 212, 1), 384); // 297 -> 384
  assert.equal(thumbPx(40, 40, 1), 64);
});

test("the wider side decides, the ratio never drops below 1, and the top bucket caps it", () => {
  assert.equal(thumbPx(100, 50, 1), 192);
  assert.equal(thumbPx(104, 104, 0.5), 192);
  assert.equal(thumbPx(900, 900, 2), 512);
});

const fake = { iconUrl: (p: string, _d: boolean, _t: boolean, s?: number) => `http://wsicon.localhost/?p=${encodeURIComponent(p)}&s=${s}&t=1` };

test("thumbUrl: the mtime rides along as v= (one exact version of the file)", () => {
  assert.equal(thumbUrl(fake, "C:\\a.jpg", 256, 1700000000123.7), "http://wsicon.localhost/?p=C%3A%5Ca.jpg&s=256&t=1&v=1700000000123");
  assert.equal(thumbUrl(fake, "C:\\a.jpg", 256, null), "http://wsicon.localhost/?p=C%3A%5Ca.jpg&s=256&t=1");
  assert.equal(thumbUrl({ iconUrl: () => "data:image/svg+xml,x" }, "a", 256, 5), "data:image/svg+xml,x", "non-http urls stay as they are");
  assert.equal(thumbUrl({ iconUrl: () => null }, "a", 256, 5), null);
});

test("thumbPair: the shared tile gets a bucket from its css size and a 32 px placeholder", () => {
  const p = thumbPair(fake, "C:\\a.jpg", 176, 132, 9);
  assert.ok(p.src!.includes(`&s=${thumbPx(176, 132)}&`));
  assert.ok(p.low!.includes(`&s=${LOW_PX}&`));
  assert.equal(thumbPair({ iconUrl: () => "same" }, "a", 176, 132).low, null, "a backend with one size has no placeholder tier");
});