/** KeyboardEvent -> the canonical hotkey combo for the recorder (PRD B9). Shares the keymap's
 *  physical-key matching, so what the user pressed on AZERTY is stored as what they meant. */

import { eventKey } from "./keymap.ts";

const LONE = new Set(["Control", "Alt", "Shift", "Meta", "Win"]);

/** The combo ("Ctrl+Alt+Space") or null when the event is not a bindable shortcut: a lone
 *  modifier, or a key with no modifier at all (only F13-F24 may go bare, like the Rust parser). */
export function eventToCombo(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): string | null {
  const key = eventKey(e);
  if (LONE.has(key) || LONE.has(e.key)) return null;
  let combo = "";
  if (e.ctrlKey) combo += "Ctrl+";
  if (e.altKey) combo += "Alt+";
  if (e.shiftKey) combo += "Shift+";
  if (e.metaKey) combo += "Win+";
  const fk = /^F(\d+)$/.exec(key);
  const bare = fk !== null && Number(fk[1]) >= 13 && Number(fk[1]) <= 24;
  if (combo === "" && !bare) return null;
  return combo + key;
}
