/** One table for every shortcut (PRD RB1): the cheat sheet (F1), the footer hints and the
 *  button tooltips all read from here, so documentation cannot drift from the handlers.
 *  Key names are physical (AZERTY-safe, RB1a): letters/digits match on KeyboardEvent.code. */

export type Scope = "popup" | "preview" | "library" | "panel" | "global";

export interface KeyDef {
  id: string;
  /** Canonical combos, e.g. "Ctrl+Shift+C". Modifiers: Ctrl, Alt, Shift, Win. */
  keys: string[];
  label: string;
  scope: Scope;
  /** Cheat-sheet group title within the scope. */
  group: string;
}

export const KEYMAP: KeyDef[] = [
  // ---- global hotkeys (rebindable in Settings › General; the cheat sheet shows the current binding)
  { id: "global.toggle", keys: ["Alt+Space"], label: "Show / hide winseek", scope: "global", group: "Global" },

  // ---- popup: moving
  { id: "popup.move", keys: ["Up", "Down"], label: "Previous / next result", scope: "popup", group: "Move" },
  { id: "popup.page", keys: ["PgUp", "PgDn"], label: "Page up / down", scope: "popup", group: "Move" },
  { id: "popup.jump", keys: ["Ctrl+Home", "Ctrl+End"], label: "First / last result", scope: "popup", group: "Move" },
  { id: "popup.section", keys: ["Ctrl+Up", "Ctrl+Down", "Ctrl+PgUp", "Ctrl+PgDn"], label: "Between sections", scope: "popup", group: "Move" },
  { id: "popup.grid", keys: ["Left", "Right"], label: "Across the image grid", scope: "popup", group: "Move" },
  { id: "popup.esc", keys: ["Esc"], label: "Back one layer (menu, preview, panel, hide)", scope: "popup", group: "Move" },

  // ---- popup: acting on the selection
  { id: "popup.open", keys: ["Enter"], label: "Open / launch", scope: "popup", group: "Selection" },
  { id: "popup.preview", keys: ["Space"], label: "Preview", scope: "popup", group: "Selection" },
  { id: "popup.reveal", keys: ["Ctrl+Enter"], label: "Show in folder", scope: "popup", group: "Selection" },
  { id: "popup.admin", keys: ["Ctrl+Shift+Enter"], label: "Run as administrator (apps)", scope: "popup", group: "Selection" },
  { id: "popup.properties", keys: ["Alt+Enter"], label: "Properties", scope: "popup", group: "Selection" },
  { id: "popup.rename", keys: ["F2"], label: "Rename", scope: "popup", group: "Selection" },
  { id: "popup.recycle", keys: ["Delete"], label: "Move to Recycle Bin", scope: "popup", group: "Selection" },
  { id: "popup.undo", keys: ["Ctrl+Z"], label: "Undo recycle", scope: "popup", group: "Selection" },
  { id: "popup.menu", keys: ["Shift+F10", "Menu"], label: "Context menu", scope: "popup", group: "Selection" },
  { id: "popup.more", keys: ["Ctrl+K"], label: "More actions (context menu)", scope: "popup", group: "Selection" },
  { id: "popup.copyPath", keys: ["Ctrl+Shift+C"], label: "Copy full path", scope: "popup", group: "Selection" },
  { id: "popup.copyFile", keys: ["Ctrl+C"], label: "Copy the file", scope: "popup", group: "Selection" },
  { id: "popup.copyName", keys: ["Ctrl+Alt+C"], label: "Copy the file name", scope: "popup", group: "Selection" },
  { id: "popup.folderOpen", keys: ["Ctrl+Shift+E"], label: "Open the folder (without selecting)", scope: "popup", group: "Selection" },
  { id: "popup.terminal", keys: ["Ctrl+Shift+T"], label: "Terminal in the folder", scope: "popup", group: "Selection" },

  // ---- popup: the query
  { id: "popup.filterTab", keys: ["Tab"], label: "Cycle type filter (Shift+Tab back)", scope: "popup", group: "Query" },
  { id: "popup.filterJump", keys: ["Alt+1", "Alt+2", "Alt+3", "Alt+4", "Alt+5", "Alt+6", "Alt+7", "Alt+8"], label: "Type filter: All, Apps, Settings, Images, Docs, Video, Audio, Folders", scope: "popup", group: "Query" },
  { id: "popup.sort", keys: ["Alt+S"], label: "Next sort order (Alt+Shift+S back)", scope: "popup", group: "Query" },
  { id: "popup.chipRemove", keys: ["Backspace"], label: "Remove the last filter chip (caret at start)", scope: "popup", group: "Query" },
  { id: "popup.history", keys: ["Alt+Up", "Alt+Down"], label: "Previous / next search", scope: "popup", group: "Query" },
  { id: "popup.showAll", keys: ["Alt+Right"], label: "Show all of this section", scope: "popup", group: "Query" },
  { id: "popup.back", keys: ["Alt+Left"], label: "Back from an expanded section", scope: "popup", group: "Query" },

  // ---- popup: windows
  { id: "popup.library", keys: ["Ctrl+L"], label: "Library", scope: "popup", group: "Windows" },
  { id: "popup.settings", keys: ["Ctrl+,"], label: "Settings", scope: "popup", group: "Windows" },
  { id: "popup.settingsStatus", keys: ["Ctrl+Shift+L"], label: "Settings › Status (index health)", scope: "popup", group: "Windows" },
  { id: "popup.cheatsheet", keys: ["F1", "Ctrl+/"], label: "This cheat sheet", scope: "popup", group: "Windows" },

  // ---- preview (open with Space)
  { id: "preview.close", keys: ["Esc"], label: "Close the preview", scope: "preview", group: "Preview" },
  { id: "preview.open", keys: ["Enter"], label: "Open the file", scope: "preview", group: "Preview" },
  { id: "preview.reveal", keys: ["Ctrl+Enter"], label: "Show in folder", scope: "preview", group: "Preview" },
  { id: "preview.copyPath", keys: ["Ctrl+Shift+C"], label: "Copy full path", scope: "preview", group: "Preview" },
  { id: "preview.menu", keys: ["Shift+F10"], label: "Context menu", scope: "preview", group: "Preview" },
  { id: "preview.page", keys: ["Ctrl+PgDn", "Ctrl+PgUp"], label: "Next / previous PDF page", scope: "preview", group: "Preview" },
  { id: "preview.zoomIn", keys: ["Ctrl+="], label: "Zoom in", scope: "preview", group: "Preview" },
  { id: "preview.zoomOut", keys: ["Ctrl+-"], label: "Zoom out", scope: "preview", group: "Preview" },
  { id: "preview.zoomFit", keys: ["Ctrl+0"], label: "Fit (again: 100 %)", scope: "preview", group: "Preview" },
  { id: "preview.pan", keys: ["Alt+Shift+Up", "Alt+Shift+Down", "Alt+Shift+Left", "Alt+Shift+Right"], label: "Pan when zoomed", scope: "preview", group: "Preview" },
  { id: "preview.extract", keys: ["Ctrl+E"], label: "Extract text / parse document", scope: "preview", group: "Preview" },
  { id: "preview.copyText", keys: ["Ctrl+Shift+X"], label: "Copy the extracted text", scope: "preview", group: "Preview" },

  // ---- library window
  { id: "library.tab", keys: ["Ctrl+1", "Ctrl+2", "Ctrl+3"], label: "Timeline / Collections / Cleanup", scope: "library", group: "Library" },
  { id: "library.back", keys: ["Esc", "Alt+Left"], label: "Back to the search", scope: "library", group: "Library" },
  { id: "library.cheatsheet", keys: ["F1", "Ctrl+/"], label: "This cheat sheet", scope: "library", group: "Library" },
  { id: "library.select", keys: ["Up", "Down"], label: "Select (timeline, collections, cleanup)", scope: "library", group: "Library" },
  { id: "library.open", keys: ["Enter"], label: "Open the selection", scope: "library", group: "Library" },
  { id: "library.new", keys: ["Ctrl+N"], label: "New collection", scope: "library", group: "Library" },
  { id: "library.edit", keys: ["F2"], label: "Rename the collection", scope: "library", group: "Library" },
  { id: "library.delete", keys: ["Delete"], label: "Delete the collection", scope: "library", group: "Library" },
  { id: "cleanup.lists", keys: ["Tab"], label: "Between the group list and the file list", scope: "library", group: "Cleanup" },
  { id: "cleanup.keep", keys: ["Space"], label: "Keep / let go of the focused file", scope: "library", group: "Cleanup" },
  { id: "cleanup.recycle", keys: ["Delete"], label: "Recycle the files let go of", scope: "library", group: "Cleanup" },

  // ---- panels (Settings, Setup, First-run, Status)
  { id: "panel.nav", keys: ["Tab"], label: "Between controls (Shift+Tab back)", scope: "panel", group: "Panels" },
  { id: "panel.sections", keys: ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7"], label: "Settings section", scope: "panel", group: "Panels" },
  { id: "panel.navArrows", keys: ["Up", "Down"], label: "Settings sections (arrow keys)", scope: "panel", group: "Panels" },
  { id: "panel.close", keys: ["Esc"], label: "Close the panel", scope: "panel", group: "Panels" },
];

// ---------------------------------------------------------------- matching

const MODS = ["Ctrl", "Alt", "Shift", "Win"] as const;

/** Canonical key name of a KeyboardEvent. Letters and digits come from `code` (physical key:
 *  on AZERTY Alt+1 arrives as key "&" on code "Digit1"), everything else from `key`. */
function eventKey(e: Pick<KeyboardEvent, "key" | "code">): string {
  const code = e.code ?? "";
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1];
  m = /^Digit([0-9])$/.exec(code);
  if (m) return m[1];
  // Symbol keys with layout-independent codes (PRD RB1a: = - / , and the numpad twins).
  if (code === "Equal" || code === "NumpadAdd") return "=";
  if (code === "Minus" || code === "NumpadSubtract") return "-";
  if (code === "Slash") return "/";
  if (code === "Comma") return ",";
  const k = e.key ?? "";
  switch (k) {
    case "Escape": return "Esc";
    case " ": return "Space";
    case "ArrowUp": return "Up";
    case "ArrowDown": return "Down";
    case "ArrowLeft": return "Left";
    case "ArrowRight": return "Right";
    case "PageUp": return "PgUp";
    case "PageDown": return "PgDn";
    case "ContextMenu": return "Menu";
    case "Apps": return "Menu";
    default: return k.length === 1 ? k.toUpperCase() : k;
  }
}

/** Does `e` match the canonical combo? Exact modifiers; letters/digits via `code`. */
export function matches(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">, combo: string): boolean {
  const parts = combo.split("+");
  const key = parts[parts.length - 1];
  if (e.ctrlKey !== parts.includes("Ctrl")) return false;
  if (e.altKey !== parts.includes("Alt")) return false;
  if (e.shiftKey !== parts.includes("Shift")) return false;
  if (e.metaKey !== parts.includes("Win")) return false;
  return eventKey(e) === key;
}

/** The first combo of `id`, pretty ("Ctrl+Shift+C"); empty when the id is unknown. */
export function hint(id: string): string {
  return KEYMAP.find((k) => k.id === id)?.keys[0] ?? "";
}

/** Every def of `scope`, grouped for the cheat sheet (stable table order within a group). */
export function byScope(scope: Scope): { group: string; items: KeyDef[] }[] {
  const out: { group: string; items: KeyDef[] }[] = [];
  for (const k of KEYMAP) {
    if (k.scope !== scope) continue;
    let g = out.find((x) => x.group === k.group);
    if (!g) out.push((g = { group: k.group, items: [] }));
    g.items.push(k);
  }
  return out;
}

/** All combos for `id` (cheat sheets show the alternates too). */
export const combos = (id: string): string[] => KEYMAP.find((k) => k.id === id)?.keys ?? [];

export { MODS, eventKey };
