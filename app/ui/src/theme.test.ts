import assert from "node:assert/strict";
import { test } from "node:test";
import {
  customThemeIdOf, customThemeRef, isCustomThemeId, loadCustomThemes, loadTheme, makeCustomThemeId,
  normalizeThemeHex, removeCustomTheme, resolveTheme, ROLES, sanitizeCustomTheme, sanitizeThemeColors,
  saveCustomThemes, saveTheme, themeMode, THEMES, upsertCustomTheme, type CustomTheme, type ThemeId,
} from "./theme.ts";

test("THEMES offers system, the two originals, and six loved palettes, with labels", () => {
  assert.deepEqual(THEMES.map((t) => t.id),
    ["system", "light", "dark", "latte", "mocha", "dracula", "tokyo", "dawn", "flexoki"]);
  assert.deepEqual(THEMES.map((t) => t.label),
    ["System", "Light", "Dark", "Latte", "Mocha", "Dracula", "Tokyo Night", "Dawn", "Flexoki"]);
});

test("themeMode: every palette sits in its family", () => {
  assert.equal(themeMode("light"), "light");
  assert.equal(themeMode("latte"), "light");
  assert.equal(themeMode("dawn"), "light");
  assert.equal(themeMode("flexoki"), "light");
  assert.equal(themeMode("dark"), "dark");
  assert.equal(themeMode("mocha"), "dark");
  assert.equal(themeMode("dracula"), "dark");
  assert.equal(themeMode("tokyo"), "dark");
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

// ---------------------------------------------------------------------------
// Custom themes: the registry, the sanitizers, and the setting that points at one.
// ---------------------------------------------------------------------------

const REC: CustomTheme = {
  id: "k3test0",
  name: "Ocean",
  base: "dark",
  accent: "#2f7cf6",
  baseAccent: "#ffc533",
  colors: { "--bg-0": "#101418", "--fg": "#dde3ea" },
  baseColors: { "--bg-0": "#1e1e2e", "--fg": "#cdd6f4" },
  createdAt: 1700000000000,
};

function recs(list: unknown): string {
  return JSON.stringify(list);
}

test("custom theme ids: the helpers round-trip and reject junk", () => {
  assert.equal(isCustomThemeId("custom:k3test0"), true);
  assert.equal(isCustomThemeId("dark"), false);
  assert.equal(isCustomThemeId("custom:"), false);
  assert.equal(isCustomThemeId("custom:UPPER"), false); // ids are lowercase
  assert.equal(isCustomThemeId("k3test0"), false);
  assert.equal(customThemeRef("k3test0"), "custom:k3test0");
  assert.equal(customThemeIdOf("custom:k3test0"), "k3test0");
  assert.equal(customThemeIdOf("dark"), null);
  const made = makeCustomThemeId();
  assert.match(made, /^[a-z0-9]{6,20}$/);
  assert.notEqual(made, makeCustomThemeId()); // two in a row differ
});

test("normalizeThemeHex: short hexes expand, junk returns null", () => {
  assert.equal(normalizeThemeHex("#abc"), "#aabbcc");
  assert.equal(normalizeThemeHex("#AABBCC"), "#aabbcc");
  assert.equal(normalizeThemeHex("#2f7cf6"), "#2f7cf6");
  assert.equal(normalizeThemeHex("2f7cf6"), null); // the # is required
  assert.equal(normalizeThemeHex("#abcd"), null);
  assert.equal(normalizeThemeHex("#ffzzzz"), null);
  assert.equal(normalizeThemeHex(7), null); // not a string at all
});

test("sanitizeThemeColors keeps only role tokens with well-formed hexes", () => {
  const clean = sanitizeThemeColors({
    "--bg-0": "#101418",
    "--fg": "#ABC",
    "--not-a-role": "#112233", // unknown token: dropped
    "--line": "red", // bad hex: dropped
  });
  assert.deepEqual(clean, { "--bg-0": "#101418", "--fg": "#aabbcc" });
  assert.deepEqual(sanitizeThemeColors("nope"), {});
  assert.deepEqual(sanitizeThemeColors(null), {});
  // every ROLES token is admissible, so a full palette can be stored
  const all: Record<string, string> = {};
  for (const r of ROLES) all[r.token] = "#112233";
  assert.equal(Object.keys(sanitizeThemeColors(all)).length, ROLES.length);
});

test("sanitizeCustomTheme repairs lenient drift, rejects headless records", () => {
  const repaired = sanitizeCustomTheme({
    id: "k3test0",
    name: "  Ocean  ",
    base: "dark",
    accent: "#BADHEX",
    colors: { "--bg-0": "#101418", "--junk": "#000000" },
    // baseColors missing: backfilled from colors
  });
  assert.ok(repaired);
  assert.equal(repaired.name, "Ocean"); // trimmed
  assert.equal(repaired.accent, "#ffc533"); // the amber default, never a bad hex
  assert.deepEqual(repaired.colors, { "--bg-0": "#101418" });
  assert.deepEqual(repaired.baseColors, { "--bg-0": "#101418" });
  assert.equal(typeof repaired.createdAt, "number");
  assert.equal(sanitizeCustomTheme({ id: "k3test0", base: "neon" }), null); // unknown base
  assert.equal(sanitizeCustomTheme({ base: "dark" }), null); // no id
  assert.equal(sanitizeCustomTheme("Ocean"), null);
  assert.equal(sanitizeCustomTheme(null), null);
  // name falls back, and the base stays a concrete palette
  const unnamed = sanitizeCustomTheme({ id: "k3test0", base: "latte" });
  assert.ok(unnamed);
  assert.equal(unnamed.name, "Custom theme");
  assert.equal(unnamed.base, "latte");
});

test("loadCustomThemes: round-trips, drops junk records, survives garbage storage", () => {
  withStorage(new Map([["winseek.customThemes", recs([REC, { id: "nonono1" }])]]), () => {
    const list = loadCustomThemes();
    assert.equal(list.length, 1); // the headless record is dropped, the good one kept
    assert.equal(list[0]?.name, "Ocean");
    assert.equal(list[0]?.colors["--bg-0"], "#101418");
  });
  withStorage(new Map([["winseek.customThemes", "{not json"]]), () => {
    assert.deepEqual(loadCustomThemes(), []);
  });
  withStorage(new Map([["winseek.customThemes", '{"a":1}']]), () => {
    assert.deepEqual(loadCustomThemes(), []); // not an array
  });
  withStorage(new Map(), () => {
    assert.deepEqual(loadCustomThemes(), []); // never set
  });
  const g = globalThis as unknown as { localStorage?: Storage };
  const prev = g.localStorage;
  g.localStorage = undefined;
  try {
    assert.deepEqual(loadCustomThemes(), []); // no storage at all
  } finally {
    g.localStorage = prev;
  }
});

test("upsertCustomTheme replaces in place, removeCustomTheme filters, saves survive", () => {
  const store = new Map<string, string>();
  const second: CustomTheme = { ...REC, id: "zzzzzz9", name: "Dusk" };
  withStorage(store, () => {
    upsertCustomTheme(REC);
    upsertCustomTheme(second);
    assert.deepEqual(loadCustomThemes().map((t) => t.id), ["k3test0", "zzzzzz9"]);
    upsertCustomTheme({ ...REC, name: "Ocean deep" });
    const list = loadCustomThemes();
    assert.equal(list.length, 2); // replaced, not appended
    assert.deepEqual(list.map((t) => t.id), ["k3test0", "zzzzzz9"]);
    assert.equal(list[0]?.name, "Ocean deep");
    assert.deepEqual(removeCustomTheme("k3test0").map((t) => t.id), ["zzzzzz9"]);
    assert.equal(store.get("winseek.customThemes"), recs([second])); // the write landed
  });
});

test("loadTheme accepts a custom reference only while its record exists", () => {
  withStorage(new Map([
    ["winseek.theme", "custom:k3test0"],
    ["winseek.customThemes", recs([REC])],
  ]), () => {
    assert.equal(loadTheme(), "custom:k3test0");
  });
  withStorage(new Map([
    ["winseek.theme", "custom:k3test0"], // the record was deleted in the other window
    ["winseek.customThemes", "[]"],
  ]), () => {
    assert.equal(loadTheme(), "system"); // dangling reference: follow the OS
  });
  withStorage(new Map([
    ["winseek.theme", "custom:k3test0"], // no registry at all: the reference counts for nothing
  ]), () => {
    assert.equal(loadTheme(), "system");
  });
  withStorage(new Map([
    ["winseek.theme", "custom:BAD_ID!"], // malformed even as a reference
  ]), () => {
    assert.equal(loadTheme(), "system");
  });
  withStorage(new Map([["winseek.theme", "custom:k3test0"], ["winseek.customThemes", recs([REC])]]), () => {
    saveTheme("custom:zzzzzz9" as ThemeId);
    assert.equal(loadTheme(), "system"); // saving a ref with no record does not stick
    saveTheme("custom:zzzzzz9" as ThemeId);
    saveCustomThemes([{ ...REC, id: "zzzzzz9", name: "Dusk" }]);
    assert.equal(loadTheme(), "custom:zzzzzz9"); // ...but it does once the record exists
  });
});
