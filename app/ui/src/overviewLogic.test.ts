import { test } from "node:test";
import assert from "node:assert/strict";
import { buildHome, buildOverview, bumpMru, chipCount, expandable, fmtMs, matchPct, nicePath, onDrive, ovMove, relDate, scopeDrive, strongName, type OvItem } from "./overviewLogic.ts";
import type { Hit, LaunchHit, LaunchItem, SemanticHit } from "./types.ts";

const isImage = (n: string) => /\.(jpe?g|png|webp)$/i.test(n);
const hit = (name: string): Hit => ({ name, path: "C:\\x\\" + name, is_dir: false, size: 1, mtime: 1 });
const sem = (name: string, source: "meaning" | "text" = "meaning"): SemanticHit => ({ path: "C:\\x\\" + name, score: 0.7, source });

test("strongName: the typed text is the name or its start", () => {
  assert.ok(strongName("Budget 2026.xlsx", "budget"));
  assert.ok(strongName("notes.txt", "notes"));
  assert.ok(!strongName("0002_email_max_length.py", "email"));
  assert.ok(!strongName("x.txt", "  "));
});

test("overview: top result, a few files, photo grid, related documents; a file is listed once", () => {
  const names = ["budget.xlsx", "budget-old.xlsx", "a.txt", "b.txt", "c.txt", "d.txt", "e.txt"].map(hit);
  const s = [sem("budget.xlsx"), ...Array.from({ length: 12 }, (_, i) => sem(`p${i}.jpg`)), sem("plan.pdf", "text")];
  const { sections, flat } = buildOverview({ names, nameTotal: 300, sem: s, text: "budget", expand: null, isImage });
  assert.deepEqual(sections.map((x) => [x.id, x.items.length, x.more]), [["top", 1, false], ["files", 4, true], ["images", 8, true], ["docs", 1, false]]);
  assert.equal(sections[1].total, 299);
  assert.equal(flat[0].hit.name, "budget.xlsx");
  assert.ok(!flat.some((x, i) => i > 0 && x.hit.name === "budget.xlsx"), "the meaning twin of the top result is not repeated");
  assert.deepEqual(flat.filter((x) => x.section === "images").map((x) => x.col), [0, 1, 2, 3, 0, 1, 2, 3]);
  assert.equal(sections[3].title, "Related by meaning");
  const wide = buildOverview({ names, nameTotal: 300, sem: s, text: "budget", expand: null, isImage, photoCols: 5 });
  assert.equal(wide.sections.find((x) => x.id === "images")?.items.length, 10, "the full window shows two rows of five");
});

test("overview: no name match -> best meaning hit is the top result; weak names are not", () => {
  const r = buildOverview({ names: [], nameTotal: 0, sem: [sem("cat.jpg"), sem("cat2.jpg")], text: "cat", expand: null, isImage });
  assert.deepEqual(r.sections.map((x) => x.id), ["top", "images"]);
  const w = buildOverview({ names: [hit("0002_email.py")], nameTotal: 1, sem: [], text: "email", expand: null, isImage });
  assert.deepEqual(w.sections.map((x) => [x.id, x.items.length]), [["files", 1]]);
});

test("overview: Show all photos lists every image and nothing else", () => {
  const s = Array.from({ length: 14 }, (_, i) => sem(`p${i}.jpg`));
  const r = buildOverview({ names: [hit("p.txt")], nameTotal: 1, sem: s, text: "p", expand: "images", isImage });
  assert.deepEqual(r.sections.map((x) => [x.id, x.items.length, x.more]), [["images", 14, false]]);
});

test("overview: a quick top-k that left relevant files out offers Show all without a number", () => {
  const s = [sem("p0.jpg"), sem("p1.jpg"), sem("notes.pdf")];
  const sec = (semMore: boolean, expand: "images" | null = null) =>
    buildOverview({ names: [hit("x.txt")], nameTotal: 1, sem: s, text: "x", expand, isImage, semMore }).sections;
  assert.deepEqual(sec(true).filter((x) => x.id === "images" || x.id === "docs").map((x) => [x.id, x.more, !!x.open]), [["images", true, true], ["docs", true, true]]);
  assert.deepEqual(sec(false).filter((x) => x.id === "images" || x.id === "docs").map((x) => [x.id, x.more, !!x.open]), [["images", false, false], ["docs", false, false]]);
  assert.deepEqual(sec(true, "images").map((x) => [x.id, x.more, !!x.open]), [["images", false, false]], "the opened section is the whole list");
});

test("overview: three or more name-matched images become photo tiles and leave Files", () => {
  const names = ["budget.xlsx", "a.png", "b.png", "c.png", "notes.txt"].map(hit);
  const r = buildOverview({ names, nameTotal: 5, sem: [], text: "budget", expand: null, isImage });
  assert.deepEqual(r.sections.map((x) => [x.id, x.items.length, x.more]), [["top", 1, false], ["files", 1, false], ["images", 3, false]]);
  const tiles = r.sections.find((x) => x.id === "images")!.items;
  assert.ok(tiles.every((x) => x.nameIdx !== undefined), "name tiles keep their row index (rename / recycle)");
  assert.deepEqual(tiles.map((x) => x.col), [0, 1, 2], "tiles keep their grid column");
});

test("overview: fewer than three name-matched images stay in Files", () => {
  const names = ["a.png", "b.png", "notes.txt"].map(hit);
  const r = buildOverview({ names, nameTotal: 3, sem: [], text: "x", expand: null, isImage });
  assert.deepEqual(r.sections.map((x) => x.id), ["files"]);
  assert.equal(r.sections[0].items.length, 3, "rows, not tiles");
});

test("overview: Show all photos also lists the name-matched images", () => {
  const names = ["budget.xlsx", ...Array.from({ length: 5 }, (_, i) => `n${i}.png`)].map(hit);
  const s = Array.from({ length: 14 }, (_, i) => sem(`p${i}.jpg`));
  const r = buildOverview({ names, nameTotal: 6, sem: s, text: "x", expand: "images", isImage });
  const imgs = r.sections.find((x) => x.id === "images")!;
  assert.deepEqual([imgs.items.length, imgs.total, imgs.more], [19, 19, false]);
  assert.equal(imgs.items[0].nameIdx, 1, "the name photos come first, keeping their row index");
  assert.equal(imgs.items[5].nameIdx, undefined, "the meaning photos follow");
});

test("ovMove walks rows, the grid by rows and tiles, and jumps sections", () => {
  const mk = (section: OvItem["section"], n: number): OvItem[] => Array.from({ length: n }, (_, i) => ({ key: section + i, section, hit: hit("f" + i), col: section === "images" ? i % 5 : undefined }));
  const C = { images: 5 };
  const flat = [...mk("top", 1), ...mk("files", 2), ...mk("images", 7), ...mk("docs", 2)];
  // indices: top 0, files 1-2, images 3-9 (row 1: 3-7, row 2: 8-9), docs 10-11
  assert.equal(ovMove(flat, 0, "down", C), 1);
  assert.equal(ovMove(flat, 2, "down", C), 3);
  assert.equal(ovMove(flat, 4, "down", C), 9, "into the shorter last row");
  assert.equal(ovMove(flat, 6, "down", C), 9);
  assert.equal(ovMove(flat, 9, "down", C), 10, "out of the grid");
  assert.equal(ovMove(flat, 10, "up", C), 8, "back into the first tile of the grid's last row");
  assert.equal(ovMove(flat, 9, "up", C), 4);
  assert.equal(ovMove(flat, 4, "up", C), 2);
  assert.equal(ovMove(flat, 7, "right", C), 7);
  assert.equal(ovMove(flat, 5, "right", C), 6);
  assert.equal(ovMove(flat, 8, "left", C), 8);
  assert.equal(ovMove(flat, 1, "left", C), 1);
  assert.equal(ovMove(flat, 1, "next", C), 3);
  assert.equal(ovMove(flat, 5, "prev", C), 3);
  assert.equal(ovMove(flat, 3, "prev", C), 1);
  assert.equal(ovMove(flat, 11, "down", C), 11);
  assert.equal(ovMove([], 3, "down", C), 0);
});

// ---- launcher sections ----
const mkLaunch = (id: string, kind: LaunchItem["kind"], title: string, score: number, target: LaunchItem["target"] = { t: "path", path: `C:\\apps\\${id}.exe`, args: null }): LaunchHit =>
  ({ item: { id, kind, title, subtitle: "test", keywords: [], target, icon: null, admin_ok: true }, score, ranges: [], confirm: false });
const app = (id: string, title: string, score: number, target?: LaunchItem["target"]) => mkLaunch(id, "app", title, score, target);
const setting = (id: string, title: string, score: number, uri: string) => mkLaunch(id, "setting", title, score, { t: "uri", uri });

test("launcher: mpv with only portable files -> portable mpv.exe is the Top result (not repeated), mpv.conf stays in Files", () => {
  const names = [hit("mpv.conf"), { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" }, hit("mpv-2.dll")];
  const r = buildOverview({ names, nameTotal: 3, sem: [], text: "mpv", expand: null, isImage, portable: [names[1]] });
  assert.equal(r.sections[0].id, "top");
  assert.equal(r.sections[0].items[0].hit.name, "mpv.exe");
  assert.ok(!r.sections.some((s) => s.id === "apps"), "the Top result is taken out of Apps");
  const files = r.sections.find((s) => s.id === "files");
  assert.deepEqual(files?.items.map((i) => i.hit.name), ["mpv.conf", "mpv-2.dll"]);
});

test("launcher: a catalog app covering the same path is listed once (as the Top result)", () => {
  const exe = { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" };
  const names = [exe];
  const launch = [app("app:mpv", "mpv", 180, { t: "aumid", aumid: "x!App", path: "D:\\tools\\mpv\\mpv.exe" })];
  const r = buildOverview({ names, nameTotal: 1, sem: [], text: "mpv", expand: null, isImage, launch, portable: [] });
  assert.equal(r.flat.filter((i) => i.hit.path === "D:\\tools\\mpv\\mpv.exe" || i.launch?.item.id === "app:mpv").length, 1);
});

test("launcher: a settings hit over TOP_MIN beats a strong file for the Top result", () => {
  const launch = [setting("set:ms-settings:bluetooth", "Bluetooth & devices", 178, "ms-settings:bluetooth")];
  const r = buildOverview({ names: [hit("bluetooth_driver.zip")], nameTotal: 1, sem: [], text: "bluetooth", expand: null, isImage, launch });
  assert.equal(r.sections[0].id, "top");
  assert.equal(r.sections[0].items[0].launch?.item.id, "set:ms-settings:bluetooth");
});

test("launcher: section order Top, Apps, Files, Windows settings, Commands; ovMove walks it", () => {
  const launch = [app("app:1", "App one", 170), setting("set:1", "Display", 160, "ms-settings:display"), mkLaunch("cmd:lock", "command", "Lock", 155, { t: "cmd", name: "lock" })];
  const r = buildOverview({ names: [hit("apply.txt")], nameTotal: 1, sem: [], text: "app", expand: null, isImage, launch });
  // the best app is the Top result and leaves its own section, so there is no Apps section here
  assert.deepEqual(r.sections.map((s) => s.id), ["top", "files", "settings", "commands"]);
  assert.equal(r.sections[2].title, "Windows settings");
  const flat = r.flat;
  assert.equal(ovMove(flat, 0, "down"), 1);
  assert.equal(ovMove(flat, 1, "next"), 2, "files -> settings");
  assert.equal(ovMove(flat, 2, "next"), 3, "settings -> commands");
  const two = buildOverview({ names: [], nameTotal: 0, sem: [], text: "app", expand: null, isImage, launch: [app("app:1", "App one", 170), app("app:2", "App two", 120)] });
  assert.deepEqual(two.sections.map((s) => s.id), ["top", "apps"]);
  assert.equal(two.sections[0].items[0].launch?.item.id, "app:1");
  assert.deepEqual(two.sections[1].items.map((i) => i.launch?.item.id), ["app:2"]);
});

test("launcher: the apps filter shows only apps, no Files section", () => {
  const launch = [app("app:1", "App one", 170)];
  const portable = [hit("tool.exe")];
  const r = buildOverview({ names: [hit("apply.txt")], nameTotal: 1, sem: [], text: "app", expand: "apps", isImage, launch, portable });
  assert.deepEqual(r.sections.map((s) => s.id), ["apps"]);
  assert.equal(r.sections[0].items.length, 2, "catalog app + portable exe");
});

test("launcher: a launch Top result's resolved exe is not listed again under Files", () => {
  const exe = { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" };
  const launch = [app("app:mpv", "mpv", 180, { t: "aumid", aumid: "x!App", path: "D:\\tools\\mpv\\mpv.exe" })];
  const r = buildOverview({ names: [exe, hit("mpv.conf")], nameTotal: 2, sem: [], text: "mpv", expand: null, isImage, launch });
  assert.equal(r.sections[0].items[0].launch?.item.id, "app:mpv");
  const files = r.sections.find((s) => s.id === "files");
  assert.ok(!files?.items.some((i) => i.hit.path === "D:\\tools\\mpv\\mpv.exe"));
});

test("expandable maps sections to their Show-all view", () => {
  const item = (section: string) => ({ key: "k", section, hit: { name: "x", path: "C:/x", is_dir: false, size: null, mtime: null } });
  assert.equal(expandable(item("files") as never), "files");
  assert.equal(expandable(item("images") as never), "images");
  assert.equal(expandable(item("apps") as never), "apps");
  assert.equal(expandable(item("settings") as never), "settings");
  assert.equal(expandable(item("docs") as never), "docs");
  assert.equal(expandable(item("top") as never), null, "the Top result has no Show all");
  assert.equal(expandable(item("commands") as never), null, "commands have no Show all");
  assert.equal(expandable(item("mostused") as never), null);
  assert.equal(expandable(undefined), null);
});

test("home: Most used grid (capped at the columns) then Recent files; ovMove walks the app grid", () => {
  const mru = Array.from({ length: 9 }, (_, i) => app("app:" + i, "App " + i, 0));
  const recent = Array.from({ length: 10 }, (_, i) => hit("r" + i));
  const h = buildHome({ mru, recent, appCols: 6, recentCap: 7 });
  assert.deepEqual(h.sections.map((x) => [x.id, x.items.length]), [["mostused", 6], ["recent", 7]]);
  assert.equal(h.flat[7].nameIdx, 1, "recent rows point into the name results");
  const C = { mostused: 6 };
  assert.equal(ovMove(h.flat, 2, "down", C), 6, "grid -> first recent row");
  assert.equal(ovMove(h.flat, 6, "up", C), 0, "recent row up -> first tile of the grid's last row");
  assert.equal(ovMove(h.flat, 1, "right", C), 2);
  assert.equal(ovMove(h.flat, 5, "right", C), 5);
  assert.equal(buildHome({ mru: [], recent: [], appCols: 6, recentCap: 7 }).sections.length, 0);
});

test("bumpMru counts launches, most used first, capped", () => {
  const it = (id: string) => app(id, id, 0).item;
  let l = bumpMru([], it("a"), 1);
  l = bumpMru(l, it("b"), 2);
  l = bumpMru(l, it("b"), 3);
  assert.deepEqual(l.map((e) => [e.item.id, e.n]), [["b", 2], ["a", 1]]);
  for (let i = 0; i < 20; i++) l = bumpMru(l, it("x" + i), 10 + i);
  assert.equal(l.length, 12);
  assert.equal(l[0].item.id, "b");
});

test("nicePath drops the home folder and joins with a chevron", () => {
  assert.deepEqual(nicePath("C:\\Users\\User\\Documents\\Finance\\2025\\inv.pdf"), { head: "Documents \u203a Finance", tail: " \u203a 2025" });
  assert.deepEqual(nicePath("C:\\Users\\User\\Documents\\inv.pdf"), { head: "", tail: "Documents" });
  assert.deepEqual(nicePath("C:\\Users\\User\\inv.pdf"), { head: "", tail: "Home" });
  assert.deepEqual(nicePath("H:\\Photos\\2025\\Paris\\a.jpg"), { head: "H: \u203a Photos \u203a 2025", tail: " \u203a Paris" });
  assert.deepEqual(nicePath("H:\\a.jpg"), { head: "", tail: "H:" });
});

test("relDate: calendar days, weeks, months, years", () => {
  const now = new Date(2026, 10, 11, 15, 0).getTime();
  const ago = (d: number) => new Date(2026, 10, 11 - d, 9, 0).getTime();
  assert.equal(relDate(ago(0), now), "Today");
  assert.equal(relDate(ago(1), now), "Yesterday");
  assert.equal(relDate(ago(4), now), "4 days ago");
  assert.equal(relDate(ago(14), now), "2 weeks ago");
  assert.equal(relDate(ago(8), now), "Last week"); // never "1 weeks ago"
  assert.equal(relDate(ago(90), now), "3 months ago");
  assert.equal(relDate(ago(900), now), "2 years ago");
  assert.equal(relDate(null, now), "");
});

test("drive scope: a path term for the name index, a prefix test for the rest", () => {
  assert.equal(scopeDrive("budget ext:xlsx", "D"), "budget ext:xlsx D:\\");
  assert.equal(scopeDrive("", "H"), "H:\\");
  assert.equal(scopeDrive("budget", ""), "budget");
  assert.ok(onDrive("D:\\x\\a.txt", "d"));
  assert.ok(!onDrive("C:\\x\\a.txt", "D"));
  assert.ok(onDrive("C:\\x\\a.txt", ""));
});

test("fmtMs: a decimal under 10 ms, whole numbers above", () => {
  assert.equal(fmtMs(0.37), "0.4");
  assert.equal(fmtMs(0), "0.1");
  assert.equal(fmtMs(42.6), "43");
});

test("chipCount keeps filter chip counts short", () => {
  assert.deepEqual([0, 842, 999, 1000, 1257, 9999, 12_345, 999_999, 1_000_000, 4_240_000].map(chipCount),
    ["0", "842", "999", "1k", "1.3k", "10k", "12k", "1M", "1M", "4.2M"]);
});

test("matchPct is the one number behind the badge and the preview pill", () => {
  assert.equal(matchPct({ score: 0.714, source: "meaning" }), 71);
  assert.equal(matchPct({ score: 0.001, source: "meaning" }), 1);
  assert.equal(matchPct({ score: 0.994, source: "meaning" }), 99);
  assert.equal(matchPct({ score: 1, source: "meaning" }), 100, "the best result of the list reads 100%");
  assert.equal(matchPct({ score: 0, source: "text" }), null);
  assert.equal(matchPct(undefined), null);
});
