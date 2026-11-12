import assert from "node:assert/strict";
import { test } from "node:test";
import { isHelperExe, launchForFilter, launchPath, pickTop, portableApps, TOP_MIN, makeLaunchGuard } from "./launchLogic.ts";
import type { Hit, LaunchHit, LaunchItem } from "./types.ts";

const hit = (path: string): Hit => {
  const name = path.slice(path.lastIndexOf("\\") + 1);
  return { name, path, is_dir: false, size: 100, mtime: 1700000000000 };
};

const launchHit = (id: string, title: string, score: number, target: LaunchItem["target"] = { t: "path", path: `C:\\apps\\${id}.exe`, args: null }, kind: LaunchItem["kind"] = "app"): LaunchHit => ({
  item: { id, kind, title, subtitle: "test", keywords: [], target, icon: null, admin_ok: true },
  score,
  ranges: [],
  confirm: false,
});

test("portable exes are promoted, helpers and uninstallers are not", () => {
  const names = [hit("C:\\Users\\a\\Downloads\\mpv.conf"), hit("D:\\tools\\mpv\\mpv.exe"), hit("D:\\tools\\mpv\\mpv-2.dll"),
    hit("C:\\x\\unins000.exe"), hit("C:\\x\\Firefox Setup.exe"), hit("C:\\x\\crashpad_handler.exe"), hit("C:\\x\\vc_redist.x64.exe")];
  const out = portableApps(names, "mpv", []);
  assert.deepEqual(out.map((h) => h.name), ["mpv.exe"]); // mpv.conf / mpv-2.dll are not exes, helpers dropped
});

test("unins000 never appears in Apps for its own query", () => {
  assert.deepEqual(portableApps([hit("C:\\x\\unins000.exe"), hit("C:\\x\\uninstall.exe")], "unins000", []), []);
});

test("portable exes already covered by a catalog app are dropped", () => {
  const exe = hit("D:\\tools\\mpv\\mpv.exe");
  const catalog = [launchHit("app:mpv", "mpv", 180, { t: "aumid", aumid: "x!App", path: "D:\\tools\\mpv\\mpv.exe" })];
  assert.deepEqual(portableApps([exe], "mpv", catalog), []);
});

test("identical stems dedupe by path preference", () => {
  const names = [hit("D:\\Downloads\\mpv\\mpv.exe"), hit("C:\\Program Files\\mpv\\mpv.exe")];
  const out = portableApps(names, "mpv", []);
  assert.deepEqual(out.map((h) => h.path), ["C:\\Program Files\\mpv\\mpv.exe"]);
});

test("a launch item over TOP_MIN takes the Top Hit from a strong file name", () => {
  const top = pickTop([launchHit("set:bt", "Bluetooth & devices", 178, { t: "uri", uri: "ms-settings:bluetooth" })],
    [hit("C:\\drv\\bluetooth_driver.zip")], "bluetooth");
  assert.equal(top.launch?.item.id, "set:bt");
});

test("below TOP_MIN the file rule still answers", () => {
  const top = pickTop([launchHit("app:x", "Xyz", 67)], [hit("C:\\x\\mpv.conf")], "mpv");
  assert.equal(top.launch, undefined);
  assert.equal(top.file?.name, "mpv.conf");
});

test("no launch hit at all: file rule decides", () => {
  const top = pickTop([], [hit("C:\\x\\mpv.conf"), hit("C:\\x\\other.txt")], "mpv");
  assert.equal(top.file?.name, "mpv.conf");
  assert.equal(top.nameIdx, 0);
  assert.ok(TOP_MIN > 0);
});

test("launcher rows follow the type filter: a folders search keeps none, apps keeps only apps", () => {
  const items = [
    launchHit("app:mpv", "mpv", 200),
    launchHit("set:bt", "Bluetooth & devices", 178, { t: "uri", uri: "ms-settings:bluetooth" }, "setting"),
    launchHit("cmd:lock", "Lock", 90, { t: "cmd", name: "lock" }, "command"),
  ];
  assert.equal(launchForFilter(items, "all").length, 3);
  assert.deepEqual(launchForFilter(items, "folders"), []); // no settings page in the Top Hit slot
  assert.deepEqual(launchForFilter(items, "images"), []);
  assert.deepEqual(launchForFilter(items, "apps").map((h) => h.item.id), ["app:mpv"]);
  // settings scope: the overview shows only the settings pages, so it needs the full list
  assert.equal(launchForFilter(items, "settings").length, 3);
});

test("launch guard: click + dblclick launches once, later launches pass", () => {
  let t = 0;
  const g = makeLaunchGuard(1500, () => t);
  assert.equal(g("app:mpc"), true);
  t = 200; assert.equal(g("app:mpc"), false); // the dblclick's own click events
  t = 300; assert.equal(g("app:other"), true);
  t = 400; assert.equal(g("app:mpc"), true);
  t = 3000; assert.equal(g("app:mpc"), true);
});

test("launchPath: the path each target kind runs, null when there is none", () => {
  const base = { id: "x", title: "X", subtitle: "Start menu", keywords: [], icon: null, admin_ok: true };
  const t = (target: object) => launchPath({ ...base, kind: "app", target } as LaunchItem);
  assert.equal(t({ t: "path", path: "D:\\my\\app.exe", args: null }), "D:\\my\\app.exe");
  assert.equal(t({ t: "exec", exe: "C:\\x\\y.exe", args: [] }), "C:\\x\\y.exe");
  assert.equal(t({ t: "aumid", aumid: "A!B", path: "C:\\store\\p.exe" }), "C:\\store\\p.exe");
  assert.equal(t({ t: "aumid", aumid: "A!B", path: null }), null); // Store app Windows knows no path for
  assert.equal(t({ t: "uri", uri: "ms-settings:display" }), "ms-settings:display");
  assert.equal(t({ t: "cmd", name: "empty-bin" }), null);
});
