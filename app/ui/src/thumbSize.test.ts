import assert from "node:assert/strict";
import { test } from "node:test";
import { thumbPx } from "./thumbSize.ts";

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
