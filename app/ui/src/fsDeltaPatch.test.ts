import { test } from "node:test";
import assert from "node:assert/strict";
import { QueryClient } from "@tanstack/react-query";
import { qk } from "./qk.ts";
import { fsDelta } from "./fsDelta.ts";
import { FsDeltaBus, patchCaches } from "./fsDeltaPatch.ts";
import type { DiskUsage, DupGroup, Hit, LibImage, UsageEntry } from "./types.ts";

const img = (path: string): LibImage => ({ path, size: 10, mtime: 0, w: 1, h: 1, class: "photo" });
const hit = (path: string): Hit => ({ name: path.slice(path.lastIndexOf("\\") + 1), path, is_dir: false, size: 5, mtime: 0 });
const ent = (path: string, size: number, is_dir = false): UsageEntry => ({ name: path.slice(path.lastIndexOf("\\") + 1), path, is_dir, kind: 1, size, files: 1, folders: 0, mtime: null });
const usage = (path: string, entries: UsageEntry[]): DiskUsage => ({ path, size: entries.reduce((a, e) => a + e.size, 0), files: entries.length, folders: 0, entries, more: 0, more_size: 0, cats: [], drives: [] });

function seeded() {
  const qc = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, gcTime: Infinity, retry: false } } });
  qc.setQueryData(qk.libImages, [img("C:\\P\\a.jpg"), img("C:\\P\\b.jpg"), img("C:\\Q\\c.jpg")]);
  qc.setQueryData(qk.libBig, [hit("C:\\P\\a.jpg"), hit("D:\\v.mkv")]);
  qc.setQueryData<DupGroup[]>(qk.libDups, [{ id: "g", members: [{ path: "C:\\P\\a.jpg", size: 1, mtime: 0, sim: null }, { path: "C:\\P\\b.jpg", size: 1, mtime: 0, sim: null }], reclaimable: 1 }]);
  qc.setQueryData(qk.libTimelineBase, 7);
  qc.setQueryData(qk.collEval("cats", "s1"), { state: "ok", hits: [{ path: "C:\\P\\a.jpg", score: 1 }, { path: "C:\\Q\\c.jpg", score: 0.9 }] });
  qc.setQueryData(qk.sem(0, "cat", "k"), { query: "cat", key: "k", hits: [{ path: "C:\\P\\a.jpg", score: 1 }], more: false, loading: false });
  qc.setQueryData(qk.spaceDir(""), usage("", [ent("C:\\", 100, true), ent("D:\\", 50, true)]));
  qc.setQueryData(qk.spaceDir("C:\\"), usage("C:\\", [ent("C:\\P", 20, true), ent("C:\\Q", 10, true)]));
  qc.setQueryData(qk.spaceDir("C:\\P"), usage("C:\\P", [ent("C:\\P\\a.jpg", 10), ent("C:\\P\\b.jpg", 10)]));
  qc.setQueryData(qk.spaceDir("C:\\Q"), usage("C:\\Q", [ent("C:\\Q\\c.jpg", 10)]));
  qc.setQueryData(qk.spaceDir("D:\\"), usage("D:\\", [ent("D:\\v.mkv", 50)]));
  return qc;
}

test("a removal patches every cached list in place and re-reads only the touched folders", () => {
  const qc = seeded();
  const before = qc.getQueryData(qk.spaceDir("D:\\"));
  const plan = patchCaches(qc, fsDelta({ removed: ["c:\\p\\A.JPG"] }));
  assert.deepEqual(qc.getQueryData<LibImage[]>(qk.libImages)!.map((i) => i.path), ["C:\\P\\b.jpg", "C:\\Q\\c.jpg"]);
  assert.deepEqual(qc.getQueryData<Hit[]>(qk.libBig)!.map((h) => h.path), ["D:\\v.mkv"]);
  assert.deepEqual(qc.getQueryData<DupGroup[]>(qk.libDups), [], "a pair that lost one is no group");
  assert.deepEqual(qc.getQueryData<{ hits: { path: string }[] }>(qk.collEval("cats", "s1"))!.hits.map((h) => h.path), ["C:\\Q\\c.jpg"]);
  assert.equal(qc.getQueryData<{ hits: unknown[] }>(qk.sem(0, "cat", "k"))!.hits.length, 0);
  const p = qc.getQueryData<DiskUsage>(qk.spaceDir("C:\\P"))!;
  assert.deepEqual(p.entries.map((e) => e.name), ["b.jpg"]);
  assert.equal(p.size, 10);
  assert.equal(qc.getQueryData(qk.spaceDir("D:\\")), before, "another drive's roll-up is untouched");
  const keys = plan.soon.map((k) => JSON.stringify(k)).sort();
  assert.deepEqual(keys, [qk.libTimelineBase, qk.spaceDir(""), qk.spaceDir("C:\\"), qk.spaceDir("C:\\P")].map((k) => JSON.stringify(k)).sort());
  assert.deepEqual(plan.later, []);
  assert.ok(!keys.includes(JSON.stringify(qk.libImages)), "a patched list is never re-read");
});

test("a folder removal drops everything below it; the removed folder's own roll-up is not re-read", () => {
  const qc = seeded();
  const plan = patchCaches(qc, fsDelta({ removed: ["C:\\Q"] }));
  assert.deepEqual(qc.getQueryData<LibImage[]>(qk.libImages)!.map((i) => i.path), ["C:\\P\\a.jpg", "C:\\P\\b.jpg"]);
  assert.deepEqual(qc.getQueryData<DiskUsage>(qk.spaceDir("C:\\"))!.entries.map((e) => e.name), ["P"]);
  assert.ok(!plan.soon.some((k) => JSON.stringify(k) === JSON.stringify(qk.spaceDir("C:\\Q"))));
});

test("the Library's own recycle (timeline hidden set) does not re-load the timeline base", () => {
  const qc = seeded();
  const plan = patchCaches(qc, fsDelta({ removed: ["C:\\P\\a.jpg"] }), { timelineCovered: true });
  assert.ok(!plan.soon.some((k) => JSON.stringify(k) === JSON.stringify(qk.libTimelineBase)));
});

test("a rename moves entries in place; a new picture re-reads the picture lists; a new non-picture only the biggest files, lazily", () => {
  const qc = seeded();
  patchCaches(qc, fsDelta({ renamed: [{ from: "C:\\Q", to: "C:\\R" }] }));
  assert.ok(qc.getQueryData<LibImage[]>(qk.libImages)!.some((i) => i.path === "C:\\R\\c.jpg"));
  const pics = patchCaches(qc, fsDelta({ added: ["C:\\P\\new.png"] }));
  const soon = pics.soon.map((k) => JSON.stringify(k));
  for (const k of [qk.libImages, qk.libDups, qk.libTimelineBase]) assert.ok(soon.includes(JSON.stringify(k)), JSON.stringify(k));
  const txt = patchCaches(qc, fsDelta({ added: ["C:\\P\\log.txt"] }));
  assert.ok(!txt.soon.some((k) => k[0] === "library"));
  assert.deepEqual(txt.later, [qk.libBig]);
});

test("overflow re-reads the old way (every library list and Space)", () => {
  const qc = seeded();
  patchCaches(qc, fsDelta({ overflow: true }));
  assert.ok(qc.getQueryState(qk.libImages)!.isInvalidated);
  assert.ok(qc.getQueryState(qk.spaceDir("D:\\"))!.isInvalidated);
  assert.deepEqual(qc.getQueryData<LibImage[]>(qk.libImages)!.length, 3, "nothing patched");
});

test("the bus coalesces re-reads across deltas and tells listeners", () => {
  const qc = seeded();
  const bus = new FsDeltaBus(qc);
  const seen: number[] = [];
  bus.on((d) => seen.push(d.removed.length));
  bus.push(fsDelta({ removed: ["C:\\P\\a.jpg"] }));
  bus.push(fsDelta({ removed: ["C:\\P\\b.jpg"] }));
  assert.deepEqual(seen, [1, 1]);
  assert.ok(!qc.getQueryState(qk.spaceDir("C:\\P"))!.isInvalidated, "nothing re-read before the window closes");
  bus.flushSoon();
  assert.ok(qc.getQueryState(qk.spaceDir("C:\\P"))!.isInvalidated);
  assert.ok(!qc.getQueryState(qk.spaceDir("D:\\"))!.isInvalidated);
  assert.ok(!qc.getQueryState(qk.libImages)!.isInvalidated);
  bus.dispose();
});
