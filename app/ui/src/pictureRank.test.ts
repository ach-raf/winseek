import { test } from "node:test";
import assert from "node:assert/strict";
import { NAME_START, NAME_WORD, nameMatch, picBadge, picReason, queryWords, rankPictures, wordQuery, wordStart } from "./pictureRank.ts";
import { buildOverview } from "./overviewLogic.ts";
import { kindScope, nameQuery, parseQuery } from "./filters.ts";
import type { Hit, SemanticHit } from "./types.ts";

const hit = (name: string): Hit => ({ name, path: "C:\\Pics\\" + name, is_dir: false, size: 1, mtime: 1 });
/** A meaning hit with its score on the gate's scale (4 = just passed). */
const sem = (name: string, rank: number, source: "meaning" | "text" = "meaning"): SemanticHit => ({ path: "C:\\Pics\\" + name, score: rank / 12, rank, source });
const isImage = (n: string) => /\.(jpe?g|png|webp|gif)$/i.test(n);
const names = (xs: { hit: Hit }[]) => xs.map((x) => x.hit.name);

test("nameMatch: a whole word, the start of a word, or letters inside another word", () => {
  const m = (n: string, q = "cat") => nameMatch(n, queryWords(q));
  for (const n of ["cat.jpg", "my cat 2021.png", "cat_food.png", "Cats.png", "black-cat.gif", "myCat.png", "IMG2021cat.jpg", "CAT (2).jpeg"]) assert.equal(m(n), "word", n);
  for (const n of ["category.png", "catalog-2024.jpg", "Catherine.png"]) assert.equal(m(n), "start", n);
  for (const n of ["intricate details.png", "location.jpg", "education.png", "scatter.png", "Bobcat.png"]) assert.equal(m(n), "part", n);
  // every typed word must be a word of the name; the weakest decides
  assert.equal(m("cat food.png", "cat food"), "word");
  assert.equal(m("cat seafood.png", "cat food"), "part");
  assert.equal(m("category food.png", "cat food"), "start");
  assert.equal(m("boxes.png", "box"), "word", "a plural ending is the same word");
  assert.equal(m("anything.png", ""), "word", "nothing typed: every name is as good as another");
  assert.equal(m("\u00c7at\u0131.png", "\u00e7at\u0131"), "word", "beyond ASCII");
});

test("wordStart is the name index's rule", () => {
  assert.ok(wordStart(" ", "c") && wordStart("_", "c") && wordStart("-", "c") && wordStart("y", "C") && wordStart("1", "c") && wordStart("t", "2"));
  assert.ok(!wordStart("i", "c") && !wordStart("C", "A") && !wordStart("1", "2"));
});

test("queryWords / wordQuery: only plain words are looked for as words", () => {
  assert.deepEqual(queryWords('Cat "Food" !dog ext:png C:\\ *.tmp size:1..2'), ["cat", "food"]);
  assert.equal(wordQuery("cat food"), "ws:cat ws:food");
  assert.equal(wordQuery("cat ext:png !dog C:\\ dm:1..2"), "ws:cat ext:png !dog C:\\ dm:1..2");
  assert.equal(wordQuery(""), "");
});

test("the intricate case: mid-word names never sit above a picture found by what it shows", () => {
  const armor = Array.from({ length: 40 }, (_, i) => ({ hit: hit(`0${i}-armor, intricate details.png`) }));
  const found = [sem("20180702_192444.jpg", 10.2), sem("photo-7.jpeg", 8.9), sem("avatar.png", 4.3)];
  const { ranked, tail } = rankPictures({ sem: found, names: armor, text: "cat" });
  assert.deepEqual(names(ranked), ["20180702_192444.jpg", "photo-7.jpeg", "avatar.png"]);
  assert.equal(tail.length, 40, "the armor pictures are kept apart");
  assert.deepEqual(ranked.map((p) => p.pct), [100, 87, 42]);
});

test("one score: both kinds of evidence first, then meaning and whole-word names by score, word starts last", () => {
  const { ranked, tail } = rankPictures({
    sem: [sem("IMG_0001.jpg", 9.5), sem("my cat.jpg", 7), sem("IMG_0002.jpg", 5.1), sem("location.jpg", 4.6), sem("scan.png", 1, "text")],
    names: ["category.png", "cat.jpg", "intricate.png", "my cat.jpg", "location.jpg"].map((n) => ({ hit: hit(n) })),
    text: "cat",
  });
  assert.deepEqual(names(ranked), ["my cat.jpg", "IMG_0001.jpg", "cat.jpg", "IMG_0002.jpg", "location.jpg", "category.png", "scan.png"]);
  assert.deepEqual(ranked.map((p) => p.score), [7 + NAME_WORD, 9.5, NAME_WORD, 5.1, 4.6, NAME_START, 1]);
  assert.deepEqual(names(tail), ["intricate.png"], "found by meaning, location.jpg is in the grid on that merit alone");
  assert.equal(ranked.find((p) => p.hit.name === "location.jpg")!.name, "part");
  assert.ok(ranked.every((p, i) => i === 0 || p.pct! <= ranked[i - 1].pct!), "the badge never rises down the list");
  assert.equal(ranked[0].pct, 100);
  assert.equal(new Set(names(ranked)).size, ranked.length, "a picture is listed once");
});

test("reasons and badges say what put a picture there", () => {
  const r = (p: Parameters<typeof picReason>[0]) => picReason(p, "cat");
  assert.equal(r({ sem: sem("a.jpg", 9), name: null }), "Matches the picture");
  assert.equal(r({ sem: sem("cat.jpg", 9), name: "word" }), "Matches the picture, and the name has \u201ccat\u201d");
  assert.equal(r({ name: "word" }), "The name has \u201ccat\u201d");
  assert.equal(r({ name: "start" }), "A word in the name starts with \u201ccat\u201d");
  assert.equal(r({ sem: sem("s.png", 1, "text"), name: null }), "Has your words in the picture");
  assert.match(r({ name: "part" })!, /inside another word/);
  assert.deepEqual([picBadge({ sem: sem("a.jpg", 9), name: "word" }), picBadge({ sem: sem("a.jpg", 9), name: "part" }), picBadge({ name: "start" }), picBadge({ sem: sem("a", 1, "text"), name: null }), picBadge({ name: "part" })],
    ["both", "picture", "name", "text", null]);
});

test("Show all keeps the order: the overview's tiles are the head of the full list, and pages only append", () => {
  const found = Array.from({ length: 30 }, (_, i) => sem(`IMG_${String(i).padStart(4, "0")}.jpg`, 10 - i * 0.2));
  const named = Array.from({ length: 50 }, (_, i) => hit(`cat ${String(i).padStart(3, "0")}.png`));
  const mid = Array.from({ length: 20 }, (_, i) => hit(`location ${i}.png`));
  // (one name row of another kind: with no name match at all the best meaning hit would be lifted out as the Top result)
  const opts = { names: [hit("cat notes.txt")], nameTotal: 1, text: "cat", isImage, photoCols: 5 };
  // overview: the quick top of each source
  const ov = buildOverview({ ...opts, sem: found.slice(0, 24), picNames: named.slice(0, 20), picTotal: 50, semMore: true, expand: null });
  const head = ov.sections.find((x) => x.id === "images")!;
  assert.equal(head.items.length, 10);
  assert.ok(head.more && head.open, "more than the quick list: Show all, without a number it cannot know yet");
  // the full list, first with one page of names, then with all of them
  const page1 = buildOverview({ ...opts, sem: found, picNames: named.slice(0, 20), picTotal: 50, picNameTotal: 70, expand: "images" });
  const full = buildOverview({ ...opts, sem: found, picNames: named, picTotal: 50, picNameTotal: 70, expand: "images" });
  const order = (r: typeof full) => r.sections.find((x) => x.id === "images")!.items.map((x) => x.hit.name);
  assert.deepEqual(order(full).slice(0, 10), head.items.map((x) => x.hit.name), "same first tiles as the overview");
  assert.equal(order(full).length, 80);
  const scoreOf = (r: typeof full) => r.sections.find((x) => x.id === "images")!.items.map((x) => x.pct!);
  assert.ok(scoreOf(full).every((p, i, a) => i === 0 || p <= a[i - 1]), "best first, all the way down");
  // a later page never reorders what was already there: page 1's tiles keep their relative order
  const pos = new Map(order(full).map((n, i) => [n, i]));
  assert.ok(order(page1).every((n, i, a) => i === 0 || pos.get(a[i - 1])! < pos.get(n)!));
  // counts stay honest: the grid counts what is ranked, the fold what only holds the letters
  const secs = (r: typeof full) => r.sections.map((x) => [x.id, x.total, x.items.length]);
  assert.deepEqual(secs(page1), [["images", 80, 50], ["namepics", 20, 0]]);
  assert.deepEqual(secs(full), [["images", 80, 80], ["namepics", 20, 0]]);
  const opened = buildOverview({ ...opts, sem: found, picNames: named, picTotal: 50, picTail: [...named, ...mid], picNameTotal: 70, expand: "images", weakOpen: true });
  assert.deepEqual(secs(opened), [["images", 80, 80], ["namepics", 20, 20]]);
  assert.match(opened.sections[1].title, /^Other names containing \u201ccat\u201d \(20\)$/);
  assert.ok(opened.sections[1].items.every((x) => x.hit.name.startsWith("location")));
});

test("an index that cannot answer word terms: the names read are sorted here, and the only results are not hidden", () => {
  const rows = ["intricate.png", "cat.jpg", "location.png"].map(hit);
  const r = buildOverview({ names: [], nameTotal: 0, sem: [], text: "cat", isImage, picNames: [], picTotal: 0, picTail: rows, picNameTotal: 3, expand: "images" });
  assert.deepEqual(r.sections.map((x) => [x.id, x.items.map((i) => i.hit.name)]), [["images", ["cat.jpg"]], ["namepics", []]]);
  const none = buildOverview({ names: [], nameTotal: 0, sem: [], text: "cat", isImage, picNames: [], picTotal: 0, picTail: [rows[0], rows[2]], picNameTotal: 2, expand: "images" });
  assert.deepEqual(none.sections.map((x) => [x.id, x.fold?.open, x.items.length]), [["namepics", true, 2]]);
});

test("chip, tab and section are one path: the same words through any of them give the same list", () => {
  const view = (query: string, tab: "all" | "images") => {
    const p = parseQuery(query, { now: Date.UTC(2026, 10, 13) });
    const scope = kindScope(p.kind, tab);
    return { scope, text: p.text, nameQ: nameQuery(p), wordQ: wordQuery(nameQuery(p)) };
  };
  const tab = view("cat", "images");
  assert.deepEqual(tab, { scope: "images", text: "cat", nameQ: "cat", wordQ: "ws:cat" });
  for (const q of ["cat image", "cat images", "image of a cat", "an image of the cat", "cat photo", "cat picture", "cat pic", "pictures of a cat", "photo of cat"]) {
    assert.deepEqual(view(q, "all"), tab, q);
    assert.deepEqual(view(q, "images"), tab, q + " with the tab on too");
  }
  assert.deepEqual(view("a photo of cats", "all"), { ...tab, text: "cats", nameQ: "cats", wordQ: "ws:cats" });
  assert.deepEqual(view("photos of the beach", "all"), { ...tab, text: "beach", nameQ: "beach", wordQ: "ws:beach" });
  assert.deepEqual(view("screenshot of an invoice", "all"), { scope: "images", text: "screenshot invoice", nameQ: "screenshot invoice", wordQ: "ws:screenshot ws:invoice" });
  assert.equal(view("cat video", "all").scope, "video");
  assert.equal(view("cat", "all").scope, "all");
  // and from there on the list is a function of (scope, text) alone: build it both ways
  const found = [sem("IMG_1.jpg", 9), sem("notes.pdf", 8)];
  const pics = ["cat.jpg", "intricate.png"].map(hit);
  const build = (query: string, tabSel: "all" | "images") => {
    const v = view(query, tabSel);
    return buildOverview({ names: pics, nameTotal: 2, sem: found, text: v.text, isImage, picNames: [pics[0]], picTotal: 1, picNameTotal: 2, expand: v.scope === "images" ? "images" : null });
  };
  assert.deepEqual(build("cat image", "all"), build("cat", "images"));
  assert.deepEqual(build("cat", "images").sections.map((x) => [x.id, x.items.map((i) => i.hit.name)]), [["images", ["IMG_1.jpg", "cat.jpg"]], ["namepics", []]]);
});
