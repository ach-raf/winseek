/** Popup view mode: the compact Spotlight popup or the full-screen layout. Pure helpers (tested). */
export type ViewMode = "compact" | "full";

export interface ViewState { default_view: ViewMode; current: ViewMode }

export const VIEW_CHOICES: { id: ViewMode; label: string; blurb: string }[] = [
  { id: "compact", label: "Compact popup", blurb: "The small Spotlight-style box in the upper third of the screen." },
  { id: "full", label: "Full screen", blurb: "Fills the screen: wide results, a preview pane, and a filter rail." },
];

/** Anything unexpected (old config, missing value) means the compact popup. */
export function parseMode(v: unknown): ViewMode {
  return v === "full" ? "full" : "compact";
}

export const toggleMode = (m: ViewMode): ViewMode => (m === "full" ? "compact" : "full");

/** F11 (no modifiers) flips between the two views. */
export function isToggleKey(e: { key: string; ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean; metaKey?: boolean }): boolean {
  return e.key === "F11" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
}

/** Class on the root shell; CSS keys the whole full-screen layout off it. */
export const shellClass = (m: ViewMode): string => (m === "full" ? " full" : "");

/** Preview pane in full mode: always on when something is selected (wider than in compact). */
export const previewWidthVar = (m: ViewMode): string | undefined => (m === "full" ? "minmax(380px, 38%)" : undefined);
