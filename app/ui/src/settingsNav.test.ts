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
  assert.deepEqual(filterSections(items, "gpu").map((s) => s.id), ["general"]);
  assert.deepEqual(filterSections(items, "full screen").map((s) => s.id), ["general"]);
  assert.deepEqual(filterSections(items, "zzz"), []);
});

test("section labels are searchable", () => {
  const named = [
    { id: "launcher" as const, label: "Apps and commands" },
    { id: "agents" as const, label: "AI agents" },
    { id: "meaning" as const, label: "Search by meaning" },
  ];
  assert.deepEqual(filterSections(named, "apps commands").map((s) => s.id), ["launcher"]);
  assert.deepEqual(filterSections(named, "ai").map((s) => s.id), ["agents"]);
  assert.deepEqual(filterSections(named, "by meaning").map((s) => s.id), ["meaning"]);
});
