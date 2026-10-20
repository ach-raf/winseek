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
