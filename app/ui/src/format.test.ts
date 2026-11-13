// Run: npm test   (node's built-in runner; Node 22.6+ strips the types)
import { test } from "node:test";
import assert from "node:assert/strict";
import { formatClock } from "./format.ts";

test("formatClock is fixed-width so ticking text never re-wraps a row", () => {
  assert.equal(formatClock(0), "0:00");
  assert.equal(formatClock(59), "0:59");
  assert.equal(formatClock(60), "1:00");
  assert.equal(formatClock(90), "1:30");
  assert.equal(formatClock(600), "10:00");
  assert.equal(formatClock(7225), "120:25");
});

test("formatClock clamps negative values", () => {
  assert.equal(formatClock(-3), "0:00");
});
