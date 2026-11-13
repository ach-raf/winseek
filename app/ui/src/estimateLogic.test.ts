import { test } from "node:test";
import assert from "node:assert/strict";
import { estimateText } from "./estimateLogic.ts";
import type { FolderEstimate } from "./types.ts";

const base: FolderEstimate = { path: "D:\Photos", files: 1200, images: 1000, pdfs: 0, docs: 200, code: 0, bytes: 5 * 1024 * 1024 * 1024,
  secs: 600, lower_bound: false, capped: false, basis: "observed", rate: 2 };

test("observed speed gives a plain estimate", () => {
  const t = estimateText(base);
  assert.match(t, /1,200 files/);
  assert.match(t, /about 10 min/);
  assert.match(t, /2\.0 files\/s/);
});

test("sampled reading time is a floor and says the meaning step is extra", () => {
  const t = estimateText({ ...base, basis: "sampled", rate: null, lower_bound: true });
  assert.match(t, /at least 10 min/);
  assert.match(t, /unknown until it has run once/);
});

test("no measurement says so instead of guessing", () => {
  const t = estimateText({ ...base, secs: null, basis: "none", rate: null, lower_bound: true });
  assert.match(t, /No time estimate yet/);
  assert.doesNotMatch(t, /min/);
});

test("empty and partial folders", () => {
  assert.match(estimateText({ ...base, files: 0, secs: 0, basis: "none", rate: null }), /Nothing to index/);
  const t = estimateText({ ...base, capped: true, lower_bound: true });
  assert.match(t, /At least 1,200 files/);
  assert.match(t, /real total is higher/);
  assert.match(estimateText({ ...base, files: 1, bytes: 10 }), /1 file \(/);
});
