/** Settings sidebar logic: remembered section, and filtering sections by a typed query. Pure (tested). */
/** Known order: app-wide behaviour first, engine next, add-ons, diagnostics last. */
export const SECTION_IDS = ["general", "indexing", "meaning", "launcher", "agents", "status"] as const;
export type SettingsSection = (typeof SECTION_IDS)[number];

/** Words that live in each section, so "hotkey" finds General and "gpu" finds Performance. */
export const SECTION_KEYWORDS: Record<SettingsSection, string> = {
  meaning: "search by meaning semantic models download embedding gemma parser pdf documents images",
  indexing: "indexing folders skipped ignored names gitignore source code index drives volumes",
  launcher: "apps and commands launcher apps start menu settings commands portable path uninstallers hidden usage history catalog",
  agents: "ai agents mcp claude tools server log connect",
  general: "general performance profile low balanced fast gpu memory vram start with windows autostart shortcuts hotkeys alt space default view full screen compact library photos",
  status: "status health service volumes files log diagnostics",
};

export function parseSection(v: unknown): SettingsSection {
  if (v === "performance") return "general"; // Performance lives in General now
  return (SECTION_IDS as readonly string[]).includes(v as string) ? (v as SettingsSection) : "general";
}

/** Sections whose label or keywords contain every typed word; an empty query keeps all. */
export function filterSections<T extends { id: SettingsSection; label: string }>(items: T[], query: string): T[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return items;
  return items.filter((s) => {
    const hay = `${s.label} ${SECTION_KEYWORDS[s.id]}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}
