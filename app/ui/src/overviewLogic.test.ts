import { test } from "node:test";
import assert from "node:assert/strict";
import { buildOverview, ovMove, strongName, GRID_COLS, type OvItem } from "./overviewLogic.ts";
import type { Hit, SemanticHit } from "./types.ts";

const isImage = (n: string) => /\.(jpe?g|png|webp)$/i.test(n);
const hit = (name: string): Hit => ({ name, path: "C:\\x\\" + name, is_dir: false, size: 1, mtime: 1 });
const sem = (name: string, source: "meaning" | "text" = "meaning"): SemanticHit => ({ path: "C:\\x\\" + name, score: 0.7, source });

test("strongName: the typed text is the name or its start", () => {
  assert.ok(strongName("Budget 2026.xlsx", "budget"));
  assert.ok(strongName("notes.txt", "notes"));
  assert.ok(!strongName("0002_email_max_length.py", "email"));
  assert.ok(!strongName("x.txt", "  "));
});

test("overview: top hit, a few files, image grid, documents; a file is listed once", () => {
  const names = ["budget.xlsx", "budget-old.xlsx", "a.txt", "b.txt", "c.txt", "d.txt", "e.txt"].map(hit);
  const s = [sem("budget.xlsx"), ...Array.from({ length: 12 }, (_, i) => sem(`p${i}.jpg`)), sem("plan.pdf", "text")];
  const { sections, flat } = buildOverview({ names, nameTotal: 300, sem: s, text: "budget", expand: null, isImage });
  assert.deepEqual(sections.map((x) => [x.id, x.items.length, x.more]), [["top", 1, false], ["files", 4, true], ["images", 10, true], ["docs", 1, false]]);
  assert.equal(sections[1].total, 299);
  assert.equal(flat[0].hit.name, "budget.xlsx");
  assert.ok(!flat.some((x, i) => i > 0 && x.hit.name === "budget.xlsx"), "the meaning twin of the top hit is not repeated");
  assert.deepEqual(flat.filter((x) => x.section === "images").map((x) => x.col), [0, 1, 2, 3, 4, 0, 1, 2, 3, 4]);
});

test("overview: no name match -> best meaning hit is the top hit; weak names are not", () => {
  const r = buildOverview({ names: [], nameTotal: 0, sem: [sem("cat.jpg"), sem("cat2.jpg")], text: "cat", expand: null, isImage });
  assert.deepEqual(r.sections.map((x) => x.id), ["top", "images"]);
  const w = buildOverview({ names: [hit("0002_email.py")], nameTotal: 1, sem: [], text: "email", expand: null, isImage });
  assert.deepEqual(w.sections.map((x) => [x.id, x.items.length]), [["files", 1]]);
});

test("overview: Show all images lists every image and nothing else", () => {
  const s = Array.from({ length: 14 }, (_, i) => sem(`p${i}.jpg`));
  const r = buildOverview({ names: [hit("p.txt")], nameTotal: 1, sem: s, text: "p", expand: "images", isImage });
  assert.deepEqual(r.sections.map((x) => [x.id, x.items.length, x.more]), [["images", 14, false]]);
});

test("ovMove walks rows, the grid by rows and tiles, and jumps sections", () => {
  const mk = (section: OvItem["section"], n: number): OvItem[] => Array.from({ length: n }, (_, i) => ({ key: section + i, section, hit: hit("f" + i), col: section === "images" ? i % GRID_COLS : undefined }));
  const flat = [...mk("top", 1), ...mk("files", 2), ...mk("images", 7), ...mk("docs", 2)];
  // indices: top 0, files 1-2, images 3-9 (row 1: 3-7, row 2: 8-9), docs 10-11
  assert.equal(ovMove(flat, 0, "down"), 1);
  assert.equal(ovMove(flat, 2, "down"), 3);
  assert.equal(ovMove(flat, 4, "down"), 9, "into the shorter last row");
  assert.equal(ovMove(flat, 6, "down"), 9);
  assert.equal(ovMove(flat, 9, "down"), 10, "out of the grid");
  assert.equal(ovMove(flat, 10, "up"), 8, "back into the first tile of the grid's last row");
  assert.equal(ovMove(flat, 9, "up"), 4);
  assert.equal(ovMove(flat, 4, "up"), 2);
  assert.equal(ovMove(flat, 7, "right"), 7);
  assert.equal(ovMove(flat, 5, "right"), 6);
  assert.equal(ovMove(flat, 8, "left"), 8);
  assert.equal(ovMove(flat, 1, "left"), 1);
  assert.equal(ovMove(flat, 1, "next"), 3);
  assert.equal(ovMove(flat, 5, "prev"), 3);
  assert.equal(ovMove(flat, 3, "prev"), 1);
  assert.equal(ovMove(flat, 11, "down"), 11);
  assert.equal(ovMove([], 3, "down"), 0);
});
