// Ready-made palettes for the custom-theme editor (Settings › Appearance › Custom themes): a
// shelf of well-loved palettes to start from. Each one is a full role set — a record seeded from
// it paints exactly like a built-in. Pure data, no imports of the app (node --test can load it).
// Light palettes have no "Rail cards" colour (that role exists on dark bases only).

import type { ConcreteTheme } from "./theme";

export type PresetPalette = {
  id: string;
  name: string;
  /** The built-in palette the record sits on: it decides light/dark chrome and the derived formulas. */
  base: ConcreteTheme;
  accent: string;
  colors: Record<string, string>;
};

type Surfaces = [canvas: string, cards: string, controls: string, line: string, rail: string, railCards: string, paper: string];
type Ink = [text: string, secondary: string, muted: string];
type Code = [keyword: string, string: string, number: string, fn: string, cls: string, deleted: string, inserted: string];

function make(id: string, name: string, base: ConcreteTheme, accent: string, s: Surfaces, t: Ink, c: Code): PresetPalette {
  const colors: Record<string, string> = {
    "--bg-0": s[0], "--bg2-0": s[1], "--bg3-0": s[2], "--line": s[3], "--side-0": s[4], "--paper": s[6],
    "--fg": t[0], "--fg2": t[1], "--fg3": t[2],
    "--tok-keyword": c[0], "--tok-string": c[1], "--tok-number": c[2], "--tok-function": c[3],
    "--tok-class": c[4], "--tok-deleted": c[5], "--tok-inserted": c[6],
  };
  if (base === "dark" || base === "mocha" || base === "dracula" || base === "tokyo") colors["--side-card-0"] = s[5];
  return { id, name, base, accent, colors };
}

export const PRESETS: PresetPalette[] = [
  // ---- dark ----
  make("nord", "Nord", "dark", "#88c0d0",
    ["#2e3440", "#272c36", "#3b4252", "#434c5e", "#242933", "#3b4252", "#eceff4"],
    ["#eceff4", "#c0c8d8", "#7b88a1"],
    ["#81a1c1", "#a3be8c", "#b48ead", "#88c0d0", "#ebcb8b", "#bf616a", "#a3be8c"]),
  make("gruvbox-dark", "Gruvbox", "dark", "#fabd2f",
    ["#282828", "#1d2021", "#3c3836", "#504945", "#1d2021", "#32302f", "#fbf1c7"],
    ["#ebdbb2", "#bdae93", "#928374"],
    ["#fb4934", "#b8bb26", "#d3869b", "#fabd2f", "#83a598", "#fb4934", "#b8bb26"]),
  make("solarized-dark", "Solarized Dark", "dark", "#b58900",
    ["#002b36", "#00212b", "#073642", "#0f4a59", "#001f27", "#073642", "#fdf6e3"],
    ["#eee8d5", "#93a1a1", "#657b83"],
    ["#859900", "#2aa198", "#d33682", "#268bd2", "#b58900", "#dc322f", "#859900"]),
  make("one-dark", "One Dark", "dark", "#61afef",
    ["#282c34", "#21252b", "#2c313a", "#3e4451", "#1e2227", "#2c313a", "#f2f3f5"],
    ["#abb2bf", "#8f96a3", "#5c6370"],
    ["#c678dd", "#98c379", "#d19a66", "#61afef", "#e5c07b", "#e06c75", "#98c379"]),
  make("rose-pine", "Rosé Pine", "dark", "#ebbcba",
    ["#191724", "#13111e", "#26233a", "#403d52", "#14121f", "#1f1d2e", "#faf4ed"],
    ["#e0def4", "#908caa", "#6e6a86"],
    ["#31748f", "#f6c177", "#eb6f92", "#9ccfd8", "#c4a7e7", "#eb6f92", "#9ccfd8"]),
  make("everforest", "Everforest", "dark", "#a7c080",
    ["#2d353b", "#272e33", "#3d484d", "#475258", "#232a2e", "#343f44", "#fdf6e3"],
    ["#d3c6aa", "#9da9a0", "#7a8478"],
    ["#e67e80", "#a7c080", "#d699b6", "#83c092", "#dbbc7f", "#e67e80", "#a7c080"]),
  make("ember", "Ember", "dark", "#a6e22e",
    ["#272822", "#1e1f1c", "#3e3d32", "#49483e", "#1a1b18", "#34352c", "#f8f8f2"],
    ["#f8f8f2", "#c1c1b5", "#75715e"],
    ["#f92672", "#e6db74", "#ae81ff", "#a6e22e", "#66d9ef", "#f92672", "#a6e22e"]),
  make("neon", "Neon", "dark", "#ff7edb",
    ["#241b2f", "#1a1325", "#34294f", "#4a3d6b", "#170f21", "#2e2342", "#fff7ff"],
    ["#f4eeff", "#b9a9d6", "#7f6fa0"],
    ["#fede5d", "#ff8b39", "#f97e72", "#36f9f6", "#72f1b8", "#fe4450", "#72f1b8"]),
  make("midnight", "Midnight", "dark", "#3b82f6",
    ["#000000", "#0a0a0a", "#1a1a1a", "#262626", "#050505", "#161616", "#f4f4f5"],
    ["#f5f5f5", "#a3a3a3", "#6b6b6b"],
    ["#ff7b72", "#a5d6a7", "#79c0ff", "#d2a8ff", "#ffa657", "#ff6b6b", "#56d364"]),
  // ---- light ----
  make("solarized-light", "Solarized Light", "light", "#268bd2",
    ["#eee8d5", "#fdf6e3", "#e3dcc5", "#cfc8b0", "#e6dfc8", "", "#fdf6e3"],
    ["#073642", "#586e75", "#93a1a1"],
    ["#859900", "#2aa198", "#d33682", "#a07800", "#268bd2", "#dc322f", "#5f7000"]),
  make("gruvbox-light", "Gruvbox Light", "light", "#d65d0e",
    ["#ebdbb2", "#fbf1c7", "#d5c4a1", "#bdae93", "#e2d2a6", "", "#fbf1c7"],
    ["#3c3836", "#504945", "#7c6f64"],
    ["#9d0006", "#79740e", "#8f3f71", "#b57614", "#076678", "#9d0006", "#79740e"]),
  make("nord-snow", "Nord Snow", "light", "#5e81ac",
    ["#e5e9f0", "#eceff4", "#d8dee9", "#c2cad8", "#d8dee9", "", "#ffffff"],
    ["#2e3440", "#4c566a", "#7b88a1"],
    ["#5e81ac", "#6f8f4e", "#8e5d8a", "#3f7f8c", "#a67c1f", "#bf616a", "#6f8f4e"]),
  make("sand", "Sand", "light", "#c2410c",
    ["#e8e2d6", "#f6f2ea", "#dcd5c6", "#c8c0ae", "#ddd6c7", "", "#fffdf8"],
    ["#2b2a26", "#5a574f", "#8c887c"],
    ["#a8591c", "#4d7c2a", "#0e7490", "#8a6d00", "#1d5fa8", "#b23b32", "#2f7a3d"]),
  make("mint", "Mint", "light", "#0f9b6c",
    ["#dcebe4", "#f1f8f5", "#cbe0d6", "#afcabc", "#cfe3d9", "", "#ffffff"],
    ["#16312a", "#3f5f55", "#7a9a8e"],
    ["#0f766e", "#3f7d2a", "#a1417a", "#8a6d00", "#1d6fa8", "#b23b32", "#2f7a3d"]),
  make("lavender", "Lavender", "light", "#7c3aed",
    ["#e6e3f2", "#f6f4fc", "#d6d2ea", "#bdb7da", "#dcd8ee", "", "#ffffff"],
    ["#2a2542", "#565078", "#8a84ab"],
    ["#7c3aed", "#2f7a3d", "#b45309", "#1d6fa8", "#be185d", "#b23b32", "#2f7a3d"]),
];

/** Accent each built-in palette ships with when used as a starting point. */
export const BUILTIN_ACCENTS: Record<string, string> = {
  light: "#ffc533", dark: "#ffc533", latte: "#1e66f5", mocha: "#cba6f7", dracula: "#bd93f9",
  tokyo: "#7aa2f7", dawn: "#d7827e", flexoki: "#24837b",
};
