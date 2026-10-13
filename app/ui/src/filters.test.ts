// Run: npm test   (node's built-in runner; Node 22.6+ strips the types)
import { test } from "node:test";
import assert from "node:assert/strict";
import { matchesRanges, nameQuery, parseQuery } from "./filters.ts";

const NOW = new Date(2026, 9, 13, 15, 30).getTime(); // Tue 13 Oct 2026
const at = (y: number, m: number, d: number) => new Date(y, m - 1, d).getTime();
const labels = (q: string, off?: Set<string>) => parseQuery(q, { now: NOW, off }).chips.map((c) => c.label);

test("photos from last summer", () => {
  const p = parseQuery("photos from last summer", { now: NOW });
  assert.equal(p.kind, "images");
  assert.equal(p.text, "");
  assert.deepEqual(p.date, { from: at(2026, 6, 1), to: at(2026, 9, 1) });
  assert.deepEqual(p.chips.map((c) => c.label), ["Images", "Summer 2026"]);
});

test("last summer in July is the previous year", () => {
  const july = new Date(2026, 6, 10).getTime();
  assert.deepEqual(parseQuery("last summer", { now: july }).date, { from: at(2025, 6, 1), to: at(2025, 9, 1) });
  assert.deepEqual(parseQuery("this summer", { now: july }).date, { from: at(2026, 6, 1), to: at(2026, 9, 1) });
});

test("winter spans the new year", () => {
  const p = parseQuery("last winter", { now: NOW });
  assert.deepEqual(p.date, { from: at(2025, 12, 1), to: at(2026, 3, 1) });
  assert.deepEqual(p.chips.map((c) => c.label), ["Winter 2025\u201326"]);
});

test("pdf invoice 2025", () => {
  const p = parseQuery("pdf invoice 2025", { now: NOW });
  assert.equal(p.ext, "pdf");
  assert.equal(p.text, "invoice");
  assert.deepEqual(p.date, { from: at(2025, 1, 1), to: at(2026, 1, 1) });
  assert.equal(nameQuery(p), "invoice ext:pdf");
  assert.deepEqual(labels("pdf invoice 2025"), ["PDF", "2025"]);
});

test("screenshots this week keeps the word as text", () => {
  const p = parseQuery("screenshots this week", { now: NOW });
  assert.equal(p.kind, "images");
  assert.equal(p.text, "screenshot");
  assert.deepEqual(p.date, { from: at(2026, 10, 12), to: at(2026, 10, 19) });
  assert.deepEqual(labels("screenshots this week"), ["Screenshots", "This week"]);
});

test("big videos", () => {
  const p = parseQuery("big videos", { now: NOW });
  assert.equal(p.kind, "video");
  assert.deepEqual(p.size, { min: 100 * 1048576, max: null });
  assert.equal(p.text, "");
  assert.deepEqual(p.chips.map((c) => c.label), ["Over 100 MB", "Video"]);
});

test("docs yesterday, folders, apps", () => {
  const p = parseQuery("docs yesterday", { now: NOW });
  assert.equal(p.kind, "docs");
  assert.deepEqual(p.date, { from: at(2026, 10, 12), to: at(2026, 10, 13) });
  assert.equal(parseQuery("folders", { now: NOW }).kind, "folders");
  assert.equal(parseQuery("apps", { now: NOW }).kind, "apps");
});

test("explicit sizes", () => {
  assert.deepEqual(parseQuery(">100mb", { now: NOW }).size, { min: 104857600, max: null });
  assert.deepEqual(parseQuery("report <5mb", { now: NOW }).size, { min: null, max: 5242880 });
  assert.deepEqual(parseQuery("> 1.5 gb", { now: NOW }).size, { min: Math.round(1.5 * 1073741824), max: null });
  assert.deepEqual(parseQuery("under 500 kb", { now: NOW }).size, { min: null, max: 512000 });
  assert.equal(parseQuery("huge", { now: NOW }).size?.min, 1073741824);
  assert.equal(parseQuery("> banana", { now: NOW }).size, null);
});

test("months, years, relative dates", () => {
  assert.deepEqual(parseQuery("march 2025", { now: NOW }).date, { from: at(2025, 3, 1), to: at(2025, 4, 1) });
  assert.deepEqual(parseQuery("in march", { now: NOW }).date, { from: at(2026, 3, 1), to: at(2026, 4, 1) });
  assert.deepEqual(parseQuery("december", { now: NOW }).date, { from: at(2025, 12, 1), to: at(2026, 1, 1) });
  assert.deepEqual(parseQuery("last month", { now: NOW }).date, { from: at(2026, 9, 1), to: at(2026, 10, 1) });
  assert.deepEqual(parseQuery("last year", { now: NOW }).date, { from: at(2025, 1, 1), to: at(2026, 1, 1) });
  assert.deepEqual(parseQuery("today", { now: NOW }).date, { from: at(2026, 10, 13), to: at(2026, 10, 14) });
  assert.deepEqual(parseQuery("last 3 days", { now: NOW }).date, { from: at(2026, 10, 10), to: at(2026, 10, 14) });
  assert.equal(parseQuery("may the force", { now: NOW }).date, null); // plain word
  assert.ok(parseQuery("from may", { now: NOW }).date);
});

test("plain queries and user syntax are untouched", () => {
  for (const q of ["invoice", "report final", "ext:pdf", '"last summer"', "!photos", "C:\Users\photos", "*.pdf"]) {
    const p = parseQuery(q, { now: NOW });
    assert.equal(p.chips.length, 0, q);
    assert.equal(p.text, q);
  }
});

test("only the first filter of each kind counts", () => {
  const p = parseQuery("photos videos 2024 2025", { now: NOW });
  assert.equal(p.kind, "images");
  assert.equal(p.text, "videos 2025");
});

test("a removed chip turns back into plain text", () => {
  const off = new Set(["kind:photos"]);
  const p = parseQuery("photos last summer", { now: NOW, off });
  assert.equal(p.kind, null);
  assert.equal(p.text, "photos");
  assert.deepEqual(p.chips.map((c) => c.label), ["Summer 2026"]);
});

test("range matching", () => {
  const p = parseQuery("big 2025", { now: NOW });
  const mid = at(2025, 6, 1);
  assert.ok(matchesRanges(p, { is_dir: false, size: 200 * 1048576, mtime: mid }));
  assert.ok(!matchesRanges(p, { is_dir: false, size: 1, mtime: mid }));
  assert.ok(!matchesRanges(p, { is_dir: false, size: 200 * 1048576, mtime: at(2026, 1, 1) }));
  assert.ok(!matchesRanges(p, { is_dir: true, size: null, mtime: mid }));
});
