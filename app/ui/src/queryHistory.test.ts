import assert from "node:assert/strict";
import { test } from "node:test";
import { push, step } from "./queryHistory.ts";

test("push rules", () => {
  assert.deepEqual(push([], "invoice"), ["invoice"]);
  assert.deepEqual(push([], "a"), [], "1-char queries are noise");
  assert.deepEqual(push([], "  "), [], "empty queries are skipped");
  // newest first, deduped, moved to front
  const l = push(["invoice", "report"], "invoice");
  assert.deepEqual(l, ["invoice", "report"]);
  assert.deepEqual(push(["invoice", "report"], "tax"), ["tax", "invoice", "report"]);
  // "same as last" stays one entry (move to front is a no-op there)
  assert.deepEqual(push(["invoice"], "invoice"), ["invoice"]);
});

test("cap at 50", () => {
  let l: string[] = [];
  for (let i = 0; i < 60; i++) l = push(l, `query-${i}`);
  assert.equal(l.length, 50);
  assert.equal(l[0], "query-59");
  assert.equal(l.includes("query-0"), false, "oldest falls off");
});

test("cursor walk with draft restore", () => {
  const l = ["two", "one"];
  // Alt+Up from the live input: newest first
  assert.deepEqual(step(l, -1, "dra", -1), { q: "two", at: 0 });
  assert.deepEqual(step(l, 0, "dra", -1), { q: "one", at: 1 });
  // stays on the oldest
  assert.deepEqual(step(l, 1, "dra", -1), { q: "one", at: 1 });
  // Alt+Down back towards the draft
  assert.deepEqual(step(l, 1, "dra", 1), { q: "two", at: 0 });
  assert.deepEqual(step(l, 0, "dra", 1), { q: "dra", at: -1 }, "past the newest: back to the draft");
  // empty history is a no-op on the draft
  assert.deepEqual(step([], -1, "dra", -1), { q: "dra", at: -1 });
});
