/** Settings sidebar logic: remembered section, and filtering sections by a typed query. Pure (tested). */
/** Known order: app-wide behaviour first, engine next, add-ons, diagnostics last. */
export const SECTION_IDS = ["general", "appearance", "shortcuts", "indexing", "meaning", "launcher", "agents", "status"] as const;
export type SettingsSection = (typeof SECTION_IDS)[number];

/** The one place section labels live; the sidebar nav and search results both render from it. */
export const SECTION_LABELS: Record<SettingsSection, string> = {
  general: "General",
  appearance: "Appearance",
  shortcuts: "Shortcuts",
  indexing: "Indexing",
  meaning: "Search by meaning",
  launcher: "Apps and commands",
  agents: "AI agents",
  status: "Status",
};

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

/** One findable setting: `id` is the row's data-setting, `title` its visible label. */
export type SettingEntry = { id: string; section: SettingsSection; title: string; keywords: string };

/** Every row the "Find a setting" box can jump to. Titles must match what the row shows;
 *  keep entries in sidebar order so equal-ranked results read in page order. */
export const SETTING_INDEX: SettingEntry[] = [
  { id: "view", section: "general", title: "Open as", keywords: "popup full window default f11 compact view" },
  { id: "autostart", section: "general", title: "Start with Windows", keywords: "autostart startup tray sign in boot launch" },
  { id: "updates", section: "general", title: "Check for updates daily", keywords: "update release check github new version" },
  { id: "version", section: "general", title: "Version", keywords: "about app number release" },
  { id: "library", section: "general", title: "Photos and clean up", keywords: "library photos open collections duplicates disk space" },
  { id: "theme", section: "appearance", title: "Theme", keywords: "light dark appearance follow windows" },
  { id: "accent", section: "appearance", title: "Accent colour", keywords: "accent color colour highlight amber coral blue green" },
  { id: "transparency", section: "appearance", title: "Window transparency", keywords: "transparency glass opacity see through alpha" },
  { id: "hotkey-popup", section: "shortcuts", title: "Shortcut", keywords: "hotkey keyboard alt space global open popup rebind" },
  { id: "hotkey-library", section: "shortcuts", title: "Open the Library", keywords: "hotkey keyboard shortcut library photos second" },
  { id: "hotkey-clipboard", section: "shortcuts", title: "Search the clipboard", keywords: "hotkey keyboard shortcut clipboard copied paste" },
  { id: "folders", section: "indexing", title: "Folders to index", keywords: "folders add remove scope drives index" },
  { id: "code", section: "indexing", title: "Index source code", keywords: "code gitignore programming files prefix" },
  { id: "ignored", section: "indexing", title: "Skipped names", keywords: "ignored excluded names wildcards filters" },
  { id: "meaning-status", section: "meaning", title: "Indexing", keywords: "meaning engine state pause resume set up progress embedding" },
  { id: "index-check", section: "meaning", title: "Index", keywords: "rebuild needed compatible stamp verdict engine update" },
  { id: "model-embedder", section: "meaning", title: "Embedding model", keywords: "download model gemma embedding vector quant" },
  { id: "model-llama", section: "meaning", title: "llama.cpp", keywords: "download engine server cuda vulkan llama build" },
  { id: "model-pdfium", section: "meaning", title: "PDF reader", keywords: "download pdf pdfium reader documents" },
  { id: "model-parser", section: "meaning", title: "Document parser", keywords: "download parser documents ocr pages scan install" },
  { id: "model-reranker", section: "meaning", title: "Result refiner", keywords: "refine rerank reranker results better match optional download install" },
  { id: "profile", section: "meaning", title: "Background work", keywords: "performance profile light balanced fast gpu vram memory graphics" },
  { id: "rebuild", section: "meaning", title: "Rebuild meaning index", keywords: "re-index reindex delete embeddings forget index again maintenance" },
  { id: "l-apps", section: "launcher", title: "Installed apps", keywords: "apps start menu store programs launcher find" },
  { id: "l-settings", section: "launcher", title: "Windows settings", keywords: "settings pages control panel launcher" },
  { id: "l-commands", section: "launcher", title: "System commands", keywords: "lock sleep restart shut down sign out recycle bin commands" },
  { id: "l-portable", section: "launcher", title: "Portable apps", keywords: "portable programs exe launcher" },
  { id: "l-path", section: "launcher", title: "Programs on your PATH", keywords: "path executables launcher tools" },
  { id: "l-uninstall", section: "launcher", title: "Hide uninstallers", keywords: "uninstallers installers hide launcher" },
  { id: "l-folders", section: "launcher", title: "Extra program folders", keywords: "folders scan programs portable launcher" },
  { id: "l-reindex", section: "launcher", title: "Re-scan apps", keywords: "catalog rescan refresh apps counts launcher" },
  { id: "l-history", section: "launcher", title: "Usage history", keywords: "history boost clear frequently used launcher" },
  { id: "a-enabled", section: "agents", title: "Let AI agents search your files", keywords: "agents mcp access read only enable claude" },
  { id: "a-content", section: "agents", title: "Let agents read file contents", keywords: "agents contents indexed any file scope" },
  { id: "a-path", section: "agents", title: "Use winseek-cli from any terminal", keywords: "cli path terminal command line environment" },
  { id: "a-connect", section: "agents", title: "Connect an agent", keywords: "connect mcp claude cursor vscode codex snippets copy" },
  { id: "a-log", section: "agents", title: "Last 50 calls", keywords: "activity log agent calls clear history" },
  { id: "s-names", section: "status", title: "Names index", keywords: "volumes drives rescan name service files diagnostics" },
  { id: "s-meaning", section: "status", title: "Meaning index details", keywords: "coverage embedded queue failed skipped speed diagnostics" },
  { id: "s-models", section: "status", title: "Models", keywords: "models server llama pdfium parser sizes diagnostics" },
];

/** Settings whose rows match every typed word. Rank: full-query title prefix, then per-word title
 *  prefix, then title contains, then keywords only; ties keep index (sidebar) order. */
export function searchSettings(query: string, limit = 8): SettingEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [];
  const hits: { e: SettingEntry; r: number }[] = [];
  for (const e of SETTING_INDEX) {
    const title = e.title.toLowerCase();
    const hay = `${title} ${e.keywords} ${SECTION_LABELS[e.section]}`.toLowerCase();
    if (!words.every((w) => hay.includes(w))) continue;
    const tWords = title.split(/\s+/);
    const w0 = words[0];
    const r = w0 != null && words.length === 1 && title.startsWith(w0) ? 0
      : words.every((w) => tWords.some((t) => t.startsWith(w))) ? 1
      : words.every((w) => title.includes(w)) ? 2
      : 3;
    hits.push({ e, r });
  }
  return hits.sort((a, b) => a.r - b.r).slice(0, limit).map((h) => h.e);
}
