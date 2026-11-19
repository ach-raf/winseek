import { test } from "node:test";
import assert from "node:assert/strict";
import { fsDelta, makeMatcher, maybeImage, patchDetail, patchUsage, pathKey, pruneGroups, pruneHits, pruneList, pruneRows, shiftIndex, touchedAncestors } from "./fsDelta.ts";
import type { DiskUsage, DupGroup, Hit, UsageEntry } from "./types.ts";

const hit = (path: string): Hit => ({ name: path.slice(path.lastIndexOf("\\") + 1), path, is_dir: false, size: 1, mtime: 0 });

test("pathKey normalises separators, case and trailing slashes", () => {
  assert.equal(pathKey("C:/Users/Me/A.JPG"), "c:\\users\\me\\a.jpg");
  assert.equal(pathKey("C:\\"), "c:");
  assert.equal(pathKey("D:\\Photos\\\\"), "d:\\photos");
});

test("isGone: exact, under a removed folder, case-insensitive, never a sibling with the same prefix", () => {
  const m = makeMatcher(fsDelta({ removed: ["C:\\Pics\\Trip", "D:/x/a.png"] }));
  assert.ok(m.isGone("c:\\pics\\trip"));
  assert.ok(m.isGone("C:\\Pics\\Trip\\day1\\IMG_1.jpg"));
  assert.ok(m.isGone("D:\\X\\A.PNG"));
  assert.ok(!m.isGone("C:\\Pics\\Trip2\\a.jpg"), "a sibling sharing the prefix stays");
  assert.ok(!m.isGone("C:\\Pics"));
});

test("a removed drive root takes everything on it; other drives stay", () => {
  const m = makeMatcher(fsDelta({ removed: ["E:\\"] }));
  assert.ok(m.isGone("E:\\a\\b.txt"));
  assert.ok(!m.isGone("F:\\a"));
});

test("a rename's old side is gone; a path re-created in the same batch (atomic save) is not", () => {
  const m = makeMatcher(fsDelta({ removed: ["C:\\d\\doc.txt"], renamed: [{ from: "C:\\d\\~tmp1", to: "C:\\d\\doc.txt" }] }));
  assert.ok(m.isGone("C:\\d\\~tmp1"));
  assert.ok(!m.isGone("C:\\d\\doc.txt"), "removed then replaced by the temp file: still there");
});

test("rewrite: files, folder prefixes (tail case kept), chains and swaps in order", () => {
  const m = makeMatcher(fsDelta({ renamed: [{ from: "C:\\Old", to: "C:\\New" }] }));
  assert.equal(m.rewrite("c:\\old\\Sub\\File.JPG"), "C:\\New\\Sub\\File.JPG");
  assert.equal(m.rewrite("C:\\Older\\x"), "C:\\Older\\x", "prefix must end at a separator");
  const same = "C:\\keep";
  assert.equal(m.rewrite(same), same);
  const chain = makeMatcher(fsDelta({ renamed: [{ from: "C:\\a", to: "C:\\b" }, { from: "C:\\b", to: "C:\\c" }] }));
  assert.equal(chain.rewrite("C:\\a"), "C:\\c");
  const swap = makeMatcher(fsDelta({ renamed: [{ from: "C:\\a", to: "C:\\t" }, { from: "C:\\b", to: "C:\\a" }, { from: "C:\\t", to: "C:\\b" }] }));
  assert.equal(swap.rewrite("C:\\a"), "C:\\b");
  assert.equal(swap.rewrite("C:\\b"), "C:\\a");
});

test("touchedAncestors: every ancestor of every changed path plus dirs, deduped, never the removed path itself", () => {
  const t = touchedAncestors(fsDelta({ removed: ["C:\\a\\b\\c.txt", "C:\\a\\b\\gone"], renamed: [{ from: "D:\\x\\1", to: "D:\\y\\2" }], dirs: ["E:\\z"] }));
  assert.deepEqual(new Set(t), new Set(["c:\\a\\b", "c:\\a", "c:", "d:\\x", "d:\\y", "d:", "e:\\z", "e:"]));
  assert.equal(t.length, new Set(t).size);
  assert.ok(!t.includes("c:\\a\\b\\gone"));
});

test("pruneList: drops gone, rewrites renamed, same array when untouched", () => {
  const items = [hit("C:\\p\\a.jpg"), hit("C:\\p\\b.jpg"), hit("C:\\q\\c.jpg")];
  const d = fsDelta({ removed: ["c:\\P\\A.JPG"], renamed: [{ from: "C:\\q", to: "C:\\r" }] });
  const out = pruneList(items, (h) => h.path, d, (h, p) => ({ ...h, path: p }));
  assert.deepEqual(out.map((h) => h.path), ["C:\\p\\b.jpg", "C:\\r\\c.jpg"]);
  assert.equal(pruneList(items, (h) => h.path, fsDelta({ removed: ["C:\\zzz"] })), items);
  assert.equal(pruneList(items, (h) => h.path, fsDelta({ overflow: true })), items, "overflow alone patches nothing");
  assert.deepEqual(pruneList(items, (h) => h.path, fsDelta({ renamed: [{ from: "C:\\q", to: "C:\\r" }] })).length, 2, "no withPath: a renamed item leaves");
});

test("pruneGroups: members leave, groups under two drop, reclaimable follows", () => {
  const mem = (path: string, size: number) => ({ path, size, mtime: 0, sim: null });
  const groups: DupGroup[] = [
    { id: "g1", members: [mem("C:\\a.jpg", 10), mem("C:\\b.jpg", 10), mem("C:\\c.jpg", 10)], reclaimable: 20 },
    { id: "g2", members: [mem("C:\\x\\1.jpg", 5), mem("C:\\y\\2.jpg", 5)], reclaimable: 5 },
    { id: "g3", members: [mem("D:\\1.jpg", 5), mem("D:\\2.jpg", 5)], reclaimable: 5 },
  ];
  const out = pruneGroups(groups, fsDelta({ removed: ["C:\\a.jpg", "C:\\x"] }));
  assert.deepEqual(out.map((g) => g.id), ["g1", "g3"]);
  assert.equal(out[0]!.reclaimable, 10);
  assert.equal(out[1], groups[2], "untouched group is the same object");
});

test("pruneHits keeps the answer object when nothing is gone", () => {
  const r = { state: "ok", hits: [{ path: "C:\\a", score: 1 }, { path: "C:\\b", score: 0.5 }] };
  assert.equal(pruneHits(r, fsDelta({ removed: ["C:\\z"] })), r);
  assert.deepEqual(pruneHits(r, fsDelta({ removed: ["C:\\a"] })).hits.map((h) => h.path), ["C:\\b"]);
});

test("patchUsage / patchDetail: children leave and their bytes come off the totals", () => {
  const ent = (name: string, size: number, dir = false): UsageEntry => ({ name, path: `C:\\x\\${name}`, is_dir: dir, kind: 1, size, files: dir ? 3 : 1, folders: dir ? 1 : 0, mtime: null });
  const d: DiskUsage = { path: "C:\\x", size: 110, files: 4, folders: 2, entries: [ent("big", 100, true), ent("f.txt", 10)], more: 0, more_size: 0, cats: [], drives: [] };
  const p = patchUsage(d, fsDelta({ removed: ["C:\\x\\big"], renamed: [{ from: "C:\\x\\f.txt", to: "C:\\x\\g.txt" }] }));
  assert.equal(p.size, 10);
  assert.equal(p.files, 1);
  assert.equal(p.folders, 0);
  assert.deepEqual(p.entries.map((e) => [e.name, e.path]), [["g.txt", "C:\\x\\g.txt"]]);
  assert.equal(patchUsage(d, fsDelta({ removed: ["C:\\elsewhere"] })), d);
  const det = { path: "C:\\x", types: [], biggest: [hit("C:\\x\\big\\v.mkv"), hit("C:\\x\\f.txt")] };
  assert.deepEqual(patchDetail(det, fsDelta({ removed: ["C:\\x\\big"] })).biggest.map((h) => h.path), ["C:\\x\\f.txt"]);
});

test("pruneRows: the sparse store shifts up past removed rows; renamed rows move in place", () => {
  const rows: (Hit | undefined)[] = [];
  rows[0] = hit("C:\\a"); rows[1] = hit("C:\\gone"); rows[2] = hit("C:\\c");
  rows[800] = hit("C:\\far"); rows[801] = hit("C:\\gone2");
  const r = pruneRows(rows, fsDelta({ removed: ["C:\\gone", "C:\\gone2"], renamed: [{ from: "C:\\c", to: "C:\\cc" }] }));
  assert.deepEqual(r.removed, [1, 801]);
  assert.equal(r.rows[1]!.path, "C:\\cc");
  assert.equal(r.rows[1]!.name, "cc");
  assert.equal(r.rows[799]!.path, "C:\\far");
  assert.equal(r.rows[800], undefined);
  assert.equal(Object.keys(r.rows).length, 3, "still sparse");
  assert.equal(pruneRows(rows, fsDelta({ removed: ["C:\\nope"] })).rows, rows);
});

test("shiftIndex: the selection follows its row, else the next, else the last", () => {
  assert.equal(shiftIndex(5, [1, 2], 10), 3);
  assert.equal(shiftIndex(5, [5], 10), 5, "selected row left: the next one takes its place");
  assert.equal(shiftIndex(9, [9], 9), 8, "last row left: the previous");
  assert.equal(shiftIndex(0, [0], 0), 0);
});

test("maybeImage: picture extensions and folders", () => {
  assert.ok(maybeImage("C:\\a\\B.JPEG"));
  assert.ok(maybeImage("C:\\a\\folder"));
  assert.ok(!maybeImage("C:\\a\\notes.txt"));
});
