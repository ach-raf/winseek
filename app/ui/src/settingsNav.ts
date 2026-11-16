/** Settings sidebar logic: remembered section, and filtering sections by a typed query. Pure (tested). */
/** Known order: app-wide behaviour first, engine next, add-ons, diagnostics last. */
export const SECTION_IDS = ["general", "appearance", "shortcuts", "indexing", "meaning", "launcher", "agents", "status"] as const;
export type SettingsSection = (typeof SECTION_IDS)[number];

/** Words that live in each section, so "hotkey" finds Shortcuts and "gpu" finds Search by meaning. */
export const SECTION_KEYWORDS: Record<SettingsSection, string> = {
  general: "general startup open as popup full window full screen view f11 start with windows autostart tray updates check version release library photos clean up duplicates",
  appearance: "appearance theme light dark follow windows accent colour color transparency glass opacity",
  shortcuts: "shortcuts hotkeys keyboard alt space global library clipboard",
  indexing: "indexing folders skipped ignored names gitignore source code index drives volumes",
  meaning: "search by meaning semantic models download embedding gemma parser pdf documents images rebuild re-index reindex download model llama pdfium pdf reader parser embedder embedding performance profile light balanced fast gpu memory vram background work pause resume set up",
  launcher: "apps and commands launcher apps start menu settings commands portable path uninstallers hidden usage history catalog",
  agents: "ai agents mcp claude tools server log connect terminal command line path winseek-cli",
  status: "status health service volumes files log diagnostics",
};

export function parseSection(v: unknown): SettingsSection {
  if (v === "performance") return "meaning"; // Performance lives in Search by meaning now
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
