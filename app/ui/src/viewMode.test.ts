import test from "node:test";
import assert from "node:assert/strict";
import { isToggleKey, parseMode, shellClass, toggleMode } from "./viewMode.ts";

test("parseMode defaults to compact", () => {
  assert.equal(parseMode("full"), "full");
  assert.equal(parseMode("compact"), "compact");
  assert.equal(parseMode(undefined), "compact");
  assert.equal(parseMode("FULL"), "compact");
  assert.equal(parseMode(3), "compact");
});

test("toggleMode flips both ways", () => {
  assert.equal(toggleMode("compact"), "full");
  assert.equal(toggleMode("full"), "compact");
});

test("F11 toggles only without modifiers", () => {
  assert.ok(isToggleKey({ key: "F11" }));
  assert.ok(!isToggleKey({ key: "F11", ctrlKey: true }));
  assert.ok(!isToggleKey({ key: "F11", altKey: true }));
  assert.ok(!isToggleKey({ key: "F10" }));
});

test("shellClass", () => {
  assert.equal(shellClass("full"), " full");
  assert.equal(shellClass("compact"), "");
});
