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

import { launchHit as lhitIgnore } from "./overviewLogic.ts";
// ---- launcher sections ----
import type { LaunchHit, LaunchItem } from "./types.ts";
const mkLaunch = (id: string, kind: LaunchItem["kind"], title: string, score: number, target: LaunchItem["target"] = { t: "path", path: `C:\\apps\\${id}.exe`, args: null }): LaunchHit =>
  ({ item: { id, kind, title, subtitle: "test", keywords: [], target, icon: null, admin_ok: true }, score, ranges: [], confirm: false });
const app = (id: string, title: string, score: number, target?: LaunchItem["target"]) => mkLaunch(id, "app", title, score, target);
const setting = (id: string, title: string, score: number, uri: string) => mkLaunch(id, "setting", title, score, { t: "uri", uri });

test("launcher: mpv with only portable files -> portable mpv.exe is Top Hit and in Apps, mpv.conf stays in Files", () => {
  const names = [hit("mpv.conf"), { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" }, hit("mpv-2.dll")];
  const r = buildOverview({ names, nameTotal: 3, sem: [], text: "mpv", expand: null, isImage, portable: [names[1]] });
  assert.equal(r.sections[0].id, "top");
  assert.equal(r.sections[0].items[0].hit.name, "mpv.exe");
  assert.ok(r.sections.some((s) => s.id === "apps" && s.items.some((i) => i.hit.name === "mpv.exe")));
  const files = r.sections.find((s) => s.id === "files");
  assert.deepEqual(files?.items.map((i) => i.hit.name), ["mpv.conf", "mpv-2.dll"]);
});

test("launcher: a catalog app covering the same path leaves one Apps row", () => {
  const exe = { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" };
  const names = [exe];
  const launch = [app("app:mpv", "mpv", 180, { t: "aumid", aumid: "x!App", path: "D:\\tools\\mpv\\mpv.exe" })];
  const r = buildOverview({ names, nameTotal: 1, sem: [], text: "mpv", expand: null, isImage, launch, portable: [] });
  // The portable list was already deduped upstream (portableApps drops covered exes); here the
  // catalog row is the only app.
  assert.equal(r.sections.find((s) => s.id === "apps")?.items.length, 1);
});

test("launcher: a settings hit over TOP_MIN beats a strong file for Top Hit", () => {
  const launch = [setting("set:ms-settings:bluetooth", "Bluetooth & devices", 178, "ms-settings:bluetooth")];
  const r = buildOverview({ names: [hit("bluetooth_driver.zip")], nameTotal: 1, sem: [], text: "bluetooth", expand: null, isImage, launch });
  assert.equal(r.sections[0].id, "top");
  assert.equal(r.sections[0].items[0].launch?.item.id, "set:ms-settings:bluetooth");
});

test("launcher: section order Top, Apps, Settings, Commands, Files; ovMove walks it", () => {
  const launch = [app("app:1", "App one", 170), setting("set:1", "Display", 160, "ms-settings:display"), mkLaunch("cmd:lock", "command", "Lock", 155, { t: "cmd", name: "lock" })];
  const r = buildOverview({ names: [hit("apply.txt")], nameTotal: 1, sem: [], text: "app", expand: null, isImage, launch });
  assert.deepEqual(r.sections.map((s) => s.id), ["top", "apps", "settings", "commands", "files"]);
  const flat = r.flat;
  assert.equal(ovMove(flat, 0, "down"), 1);
  assert.equal(ovMove(flat, 1, "next"), 2, "apps -> settings");
  assert.equal(ovMove(flat, 2, "next"), 3, "settings -> commands");
  assert.equal(ovMove(flat, 3, "next"), 4, "commands -> files");
});

test("launcher: the apps filter shows only apps, no Files section", () => {
  const launch = [app("app:1", "App one", 170)];
  const portable = [hit("tool.exe")];
  const r = buildOverview({ names: [hit("apply.txt")], nameTotal: 1, sem: [], text: "app", expand: "apps", isImage, launch, portable });
  assert.deepEqual(r.sections.map((s) => s.id), ["apps"]);
  assert.equal(r.sections[0].items.length, 2, "catalog app + portable exe");
});

test("launcher: a launch Top Hit's resolved exe is not listed again under Files", () => {
  const exe = { ...hit("mpv.exe"), path: "D:\\tools\\mpv\\mpv.exe" };
  const launch = [app("app:mpv", "mpv", 180, { t: "aumid", aumid: "x!App", path: "D:\\tools\\mpv\\mpv.exe" })];
  const r = buildOverview({ names: [exe, hit("mpv.conf")], nameTotal: 2, sem: [], text: "mpv", expand: null, isImage, launch });
  assert.equal(r.sections[0].items[0].launch?.item.id, "app:mpv");
  const files = r.sections.find((s) => s.id === "files");
  assert.ok(!files?.items.some((i) => i.hit.path === "D:\\tools\\mpv\\mpv.exe"));
});
