// The one apply path for the theme setting — boot, the storage listener, and Settings all come
// through here. theme.ts paints the palette (data-theme + a custom record's inline colours) but
// stays import-free for the node tests; this thin facade adds the one cross-module rule: a
// custom theme carries its own accent, so activating it hands that colour to the accent
// mechanism (applyAccent + saveAccent, the same path the accent picker uses). Built-in palettes
// keep whatever accent the user last chose.

import { applyTheme, customThemeRef, isCustomThemeId, loadCustomThemes, type ThemeId } from "./theme";
import { applyAccent, saveAccent } from "./accent";

export function applyThemeChoice(id: ThemeId): void {
  applyTheme(id);
  if (!isCustomThemeId(id)) return;
  const rec = loadCustomThemes().find((t) => customThemeRef(t.id) === id);
  if (!rec) return; // dangling reference: applyTheme already fell back to the OS mode
  applyAccent(rec.accent);
  saveAccent(rec.accent);
}
