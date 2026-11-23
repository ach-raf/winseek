import assert from "node:assert/strict";
import { test } from "node:test";
import { eventToCombo } from "./hotkeyLogic.ts";

const ev = (p: Partial<KeyboardEvent> & { key: string }): KeyboardEvent =>
  ({ code: "", ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...p }) as KeyboardEvent;

test("eventToCombo canonicalises on the physical key", () => {
  assert.equal(eventToCombo(ev({ key: "&", code: "Digit1", altKey: true, ctrlKey: true })), "Ctrl+Alt+1");
  assert.equal(eventToCombo(ev({ key: "l", code: "KeyL", ctrlKey: true })), "Ctrl+L");
  assert.equal(eventToCombo(ev({ key: " ", code: "Space", altKey: true })), "Alt+Space");
  assert.equal(eventToCombo(ev({ key: "F13" }), ), "F13", "bare F13-F24 is allowed");
  assert.equal(eventToCombo(ev({ key: "F24", code: "F24", shiftKey: true })), "Shift+F24");
  assert.equal(eventToCombo(ev({ key: ",", code: "Comma", ctrlKey: true })), "Ctrl+,");
});

test("eventToCombo rejects non-shortcuts", () => {
  assert.equal(eventToCombo(ev({ key: "Control" })), null, "lone modifier");
  assert.equal(eventToCombo(ev({ key: "Shift", code: "ShiftLeft" })), null);
  assert.equal(eventToCombo(ev({ key: "a", code: "KeyA" })), null, "bare letter");
  assert.equal(eventToCombo(ev({ key: "F1" })), null, "F1-F12 must carry a modifier");
  assert.equal(eventToCombo(ev({ key: "Escape" })), null, "Esc is the recorder's cancel, not a combo");
});

test("Shift alone cannot be a global shortcut (Flow.Launcher's rule)", () => {
  assert.equal(eventToCombo(ev({ key: "a", code: "KeyA", shiftKey: true })), null, "would swallow capital A everywhere");
  assert.equal(eventToCombo(ev({ key: "F5", code: "F5", shiftKey: true })), null);
  assert.equal(eventToCombo(ev({ key: "F24", code: "F24", shiftKey: true })), "Shift+F24", "F13-F24 stay free");
});

test("combos name the keycap, so AZERTY's W key is Alt+W (not its physical KeyZ)", () => {
  assert.equal(eventToCombo(ev({ key: "w", code: "KeyZ", altKey: true })), "Alt+W");
  assert.equal(eventToCombo(ev({ key: "z", code: "KeyW", altKey: true })), "Alt+Z");
  assert.equal(eventToCombo(ev({ key: "a", code: "KeyQ", ctrlKey: true })), "Ctrl+A");
});

test("punctuation records as the printed character; the backend maps it through the layout", () => {
  assert.equal(eventToCombo(ev({ key: "<", code: "Comma", altKey: true, shiftKey: true })), "Alt+Shift+<");
  assert.equal(eventToCombo(ev({ key: "<", code: "IntlBackslash", altKey: true })), "Alt+<");
  assert.equal(eventToCombo(ev({ key: ">", code: "IntlBackslash", altKey: true, shiftKey: true })), "Alt+Shift+>");
  // a non-ASCII print (AZERTY é) falls back to the positional name
  assert.equal(eventToCombo(ev({ key: "é", code: "Semicolon", ctrlKey: true })), "Ctrl+;");
  assert.equal(eventToCombo(ev({ key: "5", code: "Numpad5", ctrlKey: true })), "Ctrl+Num5");
});
