import assert from "node:assert/strict";
import { test } from "node:test";
import { loadTheme, resolveTheme, saveTheme, themeMode, THEMES, type ThemeId } from "./theme.ts";

test("THEMES offers system, the two originals, and the Catppuccin pair, with labels", () => {
  assert.deepEqual(THEMES.map((t) => t.id), ["system", "light", "dark", "latte", "mocha"]);
  assert.deepEqual(THEMES.map((t) => t.label), ["System", "Light", "Dark", "Latte", "Mocha"]);
});

test("themeMode: originals and Catppuccin sit in their families", () => {
  assert.equal(themeMode("light"), "light");
  assert.equal(themeMode("latte"), "light");
  assert.equal(themeMode("dark"), "dark");
  assert.equal(themeMode("mocha"), "dark");
});

test("resolveTheme: explicit palettes win, system follows the OS to the family original", () => {
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("light", false), "light");
  assert.equal(resolveTheme("dark", false), "dark");
  assert.equal(resolveTheme("dark", true), "dark");
  assert.equal(resolveTheme("latte", false), "latte");
  assert.equal(resolveTheme("latte", true), "latte");
  assert.equal(resolveTheme("mocha", true), "mocha");
  assert.equal(resolveTheme("mocha", false), "mocha");
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
});

/** Minimal localStorage stub so the storage code paths run for real. */
function withStorage(store: Map<string, string>, run: () => void): void {
  const g = globalThis as unknown as { localStorage?: Storage };
  const fake = {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => void store.set(k, v),
  } as unknown as Storage;
  const prev = g.localStorage;
  g.localStorage = fake;
  try { run(); } finally { g.localStorage = prev; }
}

test("loadTheme: remembers a saved theme, falls back to system", () => {
  withStorage(new Map([["winseek.theme", "dark"]]), () => {
    assert.equal(loadTheme(), "dark");
  });
  withStorage(new Map([["winseek.theme", "neon"]]), () => {
    assert.equal(loadTheme(), "system"); // garbage value: follow the OS
  });
  withStorage(new Map(), () => {
    assert.equal(loadTheme(), "system"); // never set
  });
});

test("loadTheme without any storage at all falls back to system", () => {
  const g = globalThis as unknown as { localStorage?: Storage };
  const prev = g.localStorage;
  g.localStorage = undefined;
  try {
    assert.equal(loadTheme(), "system");
  } finally {
    g.localStorage = prev;
  }
});

test("saveTheme writes the id and survives a throwing storage", () => {
  const store = new Map<string, string>();
  withStorage(store, () => {
    saveTheme("light" as ThemeId);
    assert.equal(store.get("winseek.theme"), "light");
  });
  const g = globalThis as unknown as { localStorage?: Storage };
  const prev = g.localStorage;
  g.localStorage = {
    getItem: () => null,
    setItem: () => { throw new Error("quota"); },
  } as unknown as Storage;
  try {
    saveTheme("dark"); // must not throw
    assert.equal(loadTheme(), "system"); // nothing was written
  } finally {
    g.localStorage = prev;
  }
});
