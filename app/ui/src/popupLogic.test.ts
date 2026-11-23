import assert from "node:assert/strict";
import { test } from "node:test";
import { fitMenuRect } from "./popupLogic.ts";

test("menu that fits stays at the anchor", () => {
  assert.deepEqual(fitMenuRect(100, 100, 200, 150, 1280, 800), { left: 100, top: 100 });
});

test("bottom overflow slides the menu up inside the viewport", () => {
  // y=700 + h=150 = 850 > 792 → top = 800-8-150 = 642 (bottom lands exactly on the margin)
  assert.deepEqual(fitMenuRect(100, 700, 200, 150, 1280, 800), { left: 100, top: 642 });
});

test("anchor near the bottom flips the menu above the cursor", () => {
  // the screenshot bug: tile menu at y=760 with h=220 used to clip at the window edge
  assert.deepEqual(fitMenuRect(400, 760, 190, 220, 1280, 800), { left: 400, top: 572 });
});

test("right overflow slides the menu left", () => {
  // x=1150 + w=200 = 1350 > 1272 → left = 1280-8-200 = 1072
  assert.deepEqual(fitMenuRect(1150, 100, 200, 150, 1280, 800), { left: 1072, top: 100 });
});

test("corner overflow fixes both axes", () => {
  assert.deepEqual(fitMenuRect(1150, 700, 200, 150, 1280, 800), { left: 1072, top: 642 });
});

test("exactly touching the margin is not moved (strict >)", () => {
  assert.deepEqual(fitMenuRect(100, 642, 200, 150, 1280, 800), { left: 100, top: 642 });
});

test("menu bigger than the window pins to the margin, never negative", () => {
  assert.deepEqual(fitMenuRect(10, 10, 2000, 1200, 1280, 800), { left: 8, top: 8 });
  assert.deepEqual(fitMenuRect(0, 0, 40, 40, 30, 30), { left: 8, top: 8 });
});
