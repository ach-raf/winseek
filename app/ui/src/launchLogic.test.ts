import assert from "node:assert/strict";
import { test } from "node:test";
import { isHelperExe, pickTop, portableApps, TOP_MIN } from "./launchLogic.ts";
import type { Hit, LaunchHit, LaunchItem } from "./types.ts";

const hit = (path: string): Hit => {
  const name = path.slice(path.lastIndexOf("\\") + 1);
  return { name, path, is_dir: false, size: 100, mtime: 1700000000000 };
};

const launchHit = (id: string, title: string, score: number, target: LaunchItem["target"] = { t: "path", path: `C:\\apps\\${id}.exe`, args: null }): LaunchHit => ({
  item: { id, kind: "app", title, subtitle: "test", keywords: [], target, icon: null, admin_ok: true },
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
