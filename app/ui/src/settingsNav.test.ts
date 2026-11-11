import test from "node:test";
import assert from "node:assert/strict";
import { filterSections, parseSection, SECTION_IDS } from "./settingsNav.ts";

const items = SECTION_IDS.map((id) => ({ id, label: id[0].toUpperCase() + id.slice(1) }));

test("parseSection falls back to meaning", () => {
  assert.equal(parseSection("general"), "general");
  assert.equal(parseSection("nope"), "meaning");
  assert.equal(parseSection(null), "meaning");
});

test("empty query keeps everything", () => {
  assert.equal(filterSections(items, "  ").length, items.length);
});

test("keywords find the right section", () => {
  assert.deepEqual(filterSections(items, "hotkey").map((s) => s.id), ["general"]);
  assert.deepEqual(filterSections(items, "hotkeys").map((s) => s.id), ["general"]);
  assert.deepEqual(filterSections(items, "gpu").map((s) => s.id), ["performance"]);
  assert.deepEqual(filterSections(items, "full screen").map((s) => s.id), ["general"]);
  assert.deepEqual(filterSections(items, "zzz"), []);
});
