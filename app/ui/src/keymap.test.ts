import assert from "node:assert/strict";
import { test } from "node:test";
import { byScope, combos, hint, KEYMAP, matches, type KeyDef } from "./keymap.ts";

const ev = (p: Partial<KeyboardEvent> & { key: string }): KeyboardEvent =>
  ({ code: "", ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...p }) as KeyboardEvent;

test("AZERTY: Alt+1 arrives as key & on code Digit1", () => {
  assert.equal(matches(ev({ key: "&", code: "Digit1", altKey: true }), "Alt+1"), true);
  // and the plain QWERTY shape matches too
  assert.equal(matches(ev({ key: "1", code: "Digit1", altKey: true }), "Alt+1"), true);
});

test("modifiers are exact, not supersets", () => {
  assert.equal(matches(ev({ key: "c", code: "KeyC", ctrlKey: true, shiftKey: true }), "Ctrl+Shift+C"), true);
  assert.equal(matches(ev({ key: "c", code: "KeyC", ctrlKey: true }), "Ctrl+Shift+C"), false);
  assert.equal(matches(ev({ key: "c", code: "KeyC", ctrlKey: true, shiftKey: true }), "Ctrl+C"), false);
  assert.equal(matches(ev({ key: "s", code: "KeyS", altKey: true }), "Alt+S"), true);
  assert.equal(matches(ev({ key: "s", code: "KeyS", altKey: true, shiftKey: true }), "Alt+Shift+S"), true);
  // AltGr arrives as Ctrl+Alt: must not fire Alt+S
  assert.equal(matches(ev({ key: "s", code: "KeyS", ctrlKey: true, altKey: true }), "Alt+S"), false);
});

test("F1 and Ctrl+/", () => {
  assert.equal(matches(ev({ key: "F1", code: "F1" }), "F1"), true);
  assert.equal(matches(ev({ key: "/", code: "Slash", ctrlKey: true }), "Ctrl+/"), true);
  // AZERTY: / needs Shift, but the Slash code still matches the no-shift combo
  assert.equal(matches(ev({ key: "/", code: "Slash" }), "Ctrl+/"), false, "without Ctrl it must not match");
  assert.equal(matches(ev({ key: ":", code: "Slash", ctrlKey: true, shiftKey: true }), "Ctrl+/"), false);
});

test("zoom keys match the numpad and symbol codes", () => {
  assert.equal(matches(ev({ key: "=", code: "Equal", ctrlKey: true }), "Ctrl+="), true);
  assert.equal(matches(ev({ key: "+", code: "NumpadAdd", ctrlKey: true }), "Ctrl+="), true);
  assert.equal(matches(ev({ key: "-", code: "Minus", ctrlKey: true }), "Ctrl+-"), true);
  assert.equal(matches(ev({ key: "0", code: "Digit0", ctrlKey: true }), "Ctrl+0"), true);
});

test("arrows, pgup/pgdn, menu, comma", () => {
  assert.equal(matches(ev({ key: "ArrowUp", code: "ArrowUp", altKey: true }), "Alt+Up"), true);
  assert.equal(matches(ev({ key: "ArrowRight", code: "ArrowRight", altKey: true }), "Alt+Right"), true);
  assert.equal(matches(ev({ key: "PageDown", code: "PageDown", ctrlKey: true }), "Ctrl+PgDn"), true);
  assert.equal(matches(ev({ key: "ContextMenu", code: "ContextMenu" }), "Menu"), true);
  assert.equal(matches(ev({ key: ",", ctrlKey: true }), "Ctrl+,"), true);
  assert.equal(matches(ev({ key: "Backspace", code: "Backspace" }), "Backspace"), true);
});

test("every id is unique and every def is well-formed", () => {
  const ids = new Set<string>();
  for (const k of KEYMAP) {
    assert.ok(!ids.has(k.id), `duplicate ${k.id}`);
    ids.add(k.id);
    assert.ok(k.keys.length > 0);
    assert.ok(k.label.length > 3);
    assert.ok(k.group.length > 2);
  }
});

test("the PRD's shortcut list is present (guard against quiet removal)", () => {
  const expected = [
    "global.toggle",
    "popup.move", "popup.page", "popup.jump", "popup.section", "popup.grid", "popup.esc",
    "popup.open", "popup.preview", "popup.reveal", "popup.admin", "popup.properties",
    "popup.rename", "popup.recycle", "popup.undo", "popup.menu",
    "popup.copyPath", "popup.copyFile", "popup.copyName", "popup.folderOpen", "popup.terminal",
    "popup.filterTab", "popup.filterJump", "popup.sort", "popup.chipRemove", "popup.history",
    "popup.showAll", "popup.back",
    "popup.library", "popup.settings", "popup.settingsStatus", "popup.cheatsheet",
    "preview.close", "preview.open", "preview.reveal", "preview.copyPath", "preview.menu",
    "preview.page", "preview.zoomIn", "preview.zoomOut", "preview.zoomFit", "preview.pan",
    "preview.extract", "preview.copyText",
    "library.tab", "library.back", "library.cheatsheet", "library.select", "library.open",
    "library.new", "library.edit", "library.delete",
    "cleanup.lists", "cleanup.keep", "cleanup.recycle",
    "panel.nav", "panel.sections", "panel.navArrows", "panel.close",
  ];
  const have = new Set(KEYMAP.map((k) => k.id));
  for (const id of expected) assert.ok(have.has(id), `missing ${id}`);
  // AltGr safety (PRD RB1a): no Ctrl+Alt+<letter a-z except C> bound in the popup — AltGr+letter
  // types a character on EU layouts (e.g. AltGr+E = €).
  for (const k of KEYMAP) {
    for (const combo of k.keys) {
      const m = /^Ctrl\+Alt\+([A-Z])$/.exec(combo);
      if (m) assert.equal(m[1], "C", `Ctrl+Alt+${m[1]} collides with AltGr (${k.id})`);
    }
  }
});

test("hint and byScope helpers", () => {
  assert.equal(hint("popup.copyPath"), "Ctrl+Shift+C");
  assert.equal(hint("nope"), "");
  assert.deepEqual(combos("popup.cheatsheet"), ["F1", "Ctrl+/"]);
  const popup = byScope("popup");
  assert.ok(popup.length >= 4);
  for (const g of popup) for (const item of g.items) assert.equal(item.scope, "popup");
  const all: KeyDef[] = byScope("library").flatMap((g) => g.items);
  assert.ok(all.length > 5);
});
