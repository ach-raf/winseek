import { test } from "node:test";
import assert from "node:assert/strict";
import { CountWatchdog, GRACE_MS, MIN_GAP_MS, statusSignature } from "./fsWatchdog.ts";
import type { VolumeStatus } from "./types.ts";

const vol = (files: number, state = "live", letter = "C"): VolumeStatus => ({ letter, files, folders: 10, state });

test("the signature changes with counts and volumes", () => {
  assert.notEqual(statusSignature([vol(1)]), statusSignature([vol(2)]));
  assert.notEqual(statusSignature([vol(1)]), statusSignature([vol(1), vol(1, "live", "D")]));
});

test("a count change right after a delta is explained: no re-read", () => {
  const w = new CountWatchdog();
  assert.equal(w.onStatus([vol(100)], 0), false);
  w.onDelta(10_000);
  assert.equal(w.onStatus([vol(99)], 11_000), false);
  assert.equal(w.onStatus([vol(99)], 11_000 + GRACE_MS + 1), false);
});

test("a count change with no delta trips after the grace period, once", () => {
  const w = new CountWatchdog();
  w.onStatus([vol(100)], 0);
  assert.equal(w.onStatus([vol(90)], 20_000), false); // seen, grace starts
  assert.equal(w.onStatus([vol(90)], 20_000 + GRACE_MS - 1), false);
  assert.equal(w.onStatus([vol(90)], 20_000 + GRACE_MS), true);
  assert.equal(w.onStatus([vol(90)], 20_000 + GRACE_MS + 1000), false); // already fired
});

test("a late delta inside the grace period clears the suspicion", () => {
  const w = new CountWatchdog();
  w.onStatus([vol(100)], 0);
  w.onStatus([vol(90)], 20_000);
  w.onDelta(21_000);
  assert.equal(w.onStatus([vol(90)], 20_000 + GRACE_MS + 500), false);
});

test("scans never trip it and re-reads keep a minimum gap", () => {
  const w = new CountWatchdog();
  w.onStatus([vol(100)], 0);
  assert.equal(w.onStatus([vol(500, "scanning")], 20_000), false);
  assert.equal(w.onStatus([vol(900, "live")], 25_000), false); // baseline after the scan
  w.onStatus([vol(800)], 40_000);
  assert.equal(w.onStatus([vol(800)], 40_000 + GRACE_MS), true);
  w.onStatus([vol(700)], 40_000 + GRACE_MS + 100);
  assert.equal(w.onStatus([vol(700)], 40_000 + GRACE_MS + 100 + GRACE_MS), false); // inside MIN_GAP_MS
  assert.equal(w.onStatus([vol(700)], 40_000 + GRACE_MS + MIN_GAP_MS + 1), true);
});
