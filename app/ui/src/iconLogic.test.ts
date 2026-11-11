import { test } from "node:test";
import assert from "node:assert/strict";
import { appGradient, extLabel, glyphFor, initials } from "./iconLogic.ts";

test("glyphFor: badge colour by file type, label = extension", () => {
  assert.deepEqual(glyphFor("Invoice.PDF"), { col: "#e5484d", lab: "PDF" });
  assert.deepEqual(glyphFor("budget.xlsx"), { col: "#1f9d55", lab: "XLSX" });
  assert.deepEqual(glyphFor("main.rs"), { col: "#475569", lab: "RS" });
  assert.deepEqual(glyphFor("a.mp3").col, "#d6409f");
  assert.deepEqual(glyphFor("a.mkv").col, "#7c5ce0");
  assert.deepEqual(glyphFor("a.7z").col, "#b7791f");
  assert.deepEqual(glyphFor("weird.abcdefg"), { col: "#6b7280", lab: "ABCD" });
  assert.deepEqual(glyphFor("README"), { col: "#6b7280", lab: "" });
});

test("extLabel ignores dotfiles and trailing dots", () => {
  assert.equal(extLabel(".gitignore"), "");
  assert.equal(extLabel("a."), "");
  assert.equal(extLabel("a.tar.gz"), "GZ");
});

test("initials: two words -> two capitals, one word -> Capital + lower", () => {
  assert.equal(initials("Visual Studio Code"), "VS");
  assert.equal(initials("Windows Terminal"), "WT");
  assert.equal(initials("Figma"), "Fi");
  assert.equal(initials("mpv"), "Mp");
  assert.equal(initials("7-Zip"), "7Z");
  assert.equal(initials("X"), "X");
  assert.equal(initials("++"), "+");
});

test("appGradient is stable per name and differs between names", () => {
  assert.deepEqual(appGradient("Figma"), appGradient("figma"));
  assert.notDeepEqual(appGradient("Figma"), appGradient("Notepad++"));
  assert.match(appGradient("Figma")[0], /^hsl\(\d+ 72% 60%\)$/);
});
