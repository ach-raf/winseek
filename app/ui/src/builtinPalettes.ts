// The built-in palettes as role colours, read straight from base.css (the single source of
// truth) so "start from Mocha" always matches what Mocha really looks like. Light palettes fall
// back to the :root block for roles their own block leaves to it.

import baseCss from "./base.css?raw";
import { ROLES, THEMES, normalizeThemeHex, themeMode, type ConcreteTheme } from "./theme";
import { BUILTIN_ACCENTS, type PresetPalette } from "./themePresets";

function block(sel: RegExp): string {
  const m = sel.exec(baseCss);
  if (!m) return "";
  const end = baseCss.indexOf("}", m.index);
  return baseCss.slice(m.index, end);
}

function readRoles(css: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of ROLES) {
    const m = new RegExp(`${r.token}:\\s*(#[0-9a-fA-F]{3,6})\\b`).exec(css);
    const hex = m ? normalizeThemeHex(m[1]) : null;
    if (hex) out[r.token] = hex;
  }
  return out;
}

let cache: PresetPalette[] | null = null;

export function builtinPalettes(): PresetPalette[] {
  if (cache) return cache;
  const root = readRoles(block(/^:root\s*\{/m));
  cache = THEMES.filter((t) => t.id !== "system").map((t) => {
    const id = t.id as ConcreteTheme;
    const own = id === "light" ? {} : readRoles(block(new RegExp(`^:root\\[data-theme="${id}"\\]\\s*\\{`, "m")));
    const dark = themeMode(id) === "dark";
    const colors = { ...(dark ? {} : root), ...own };
    if (!dark) delete colors["--side-card-0"];
    return { id: `builtin-${id}`, name: t.label, base: id, accent: BUILTIN_ACCENTS[id] ?? "#ffc533", colors };
  });
  return cache;
}
