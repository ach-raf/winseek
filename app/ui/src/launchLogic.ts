import type { Filter, Hit, LaunchHit } from "./types";

// Pure helpers for the launcher integration (PRD R9 + §4.1). No imports besides types so the
// rules stay unit-testable — App.tsx feeds it name hits + launch hits per keystroke.

/** Exe stems that are installers / helpers, never apps. Same list as `launch/appsrules.rs`
 *  (is_helper_exe) — keep both lists identical. */
const HELPER_PATTERNS = ["setup", "-installer", "updater", "crashpad", "vc_redist"];

export function isHelperExe(stem: string): boolean {
  const s = stem.toLowerCase();
  if (!s) return false;
  return HELPER_PATTERNS.some((p) =>
    p === "setup" ? s.startsWith(p) || s.includes(" setup")
    : p === "crashpad" || p === "vc_redist" ? s.startsWith(p)
    : p === "-installer" ? s.endsWith(p)
    : s.includes(p),
  );
}

/** Directories a portable app never comes from. */
const BAD_DIR = /[\\](?:Windows[\\]WinSxS|Windows[\\]Installer|\$Recycle\.Bin|Temp)[\\]/i;

const stemOf = (name: string) => {
  const d = name.lastIndexOf(".");
  return (d > 0 ? name.slice(0, d) : name);
};

/** The stem "strongly matches" the query: equal, prefix, word start, or CamelCase acronym. */
function stemMatches(stem: string, q: string): boolean {
  const s = stem.toLowerCase();
  if (s === q || s.startsWith(q)) return true;
  if (s.split(/[\s\-_.]+/).some((w) => w.startsWith(q))) return true;
  // Acronym of CamelCase / spaced words ("VisualStudioCode" -> "vsc").
  const initials = (s.match(/[A-Z]|^[a-z]+|[0-9]+/g) ?? []).map((w) => w[0]).join("").toLowerCase();
  return initials === q && q.length > 1;
}

/** Dedupe rank: Program Files first, then scoop's apps\*\current, then the shorter path. */
function pathRank(path: string): number {
  const p = path.toLowerCase();
  if (p.includes("\\program files")) return 0;
  if (/[\\]scoop[\\]apps[\\][^\\]+[\\]current[\\]/.test(p)) return 1;
  return 2 + Math.min(1000, path.length / 100);
}

/**
 * Name-index hits promoted into the Apps section (PRD R9): exes whose stem strongly matches
 * the query, minus installers/helpers/system locations, minus anything a catalog app already
 * covers, deduped per stem by path preference.
 */
export function portableApps(names: Hit[], text: string, launch: LaunchHit[]): Hit[] {
  const q = text.trim().toLowerCase();
  if (!q) return [];
  const taken = new Set<string>();
  for (const l of launch) {
    const t = l.item.target;
    if (t.t === "path") taken.add(t.path.toLowerCase());
    else if (t.t === "aumid" && t.path) taken.add(t.path.toLowerCase());
  }
  const byStem = new Map<string, { hit: Hit; rank: number }>();
  for (const h of names) {
    if (h.is_dir || !/\.exe$/i.test(h.name)) continue;
    const stem = stemOf(h.name);
    // ^unins is uninstaller territory (appsrules::is_uninstaller); the rest is the helper list.
    if (/^unins/i.test(stem)) continue;
    if (!stemMatches(stem, q) || isHelperExe(stem) || BAD_DIR.test(h.path)) continue;
    if (taken.has(h.path.toLowerCase())) continue;
    const rank = pathRank(h.path);
    const key = stem.toLowerCase();
    const prev = byStem.get(key);
    if (!prev || rank < prev.rank) byStem.set(key, { hit: h, rank });
  }
  return [...byStem.values()].sort((a, b) => a.rank - b.rank).map((v) => v.hit);
}

/** A launch item scores this high (after its history boost) to take the Top Hit slot. */
export const TOP_MIN = 150;

/**
 * Which launcher hits belong in the overview for the active type filter. A file-type filter
 * (Folders, Images, …) means the user asked for files of that kind, so launcher rows — the
 * Top Hit pick included — stand down; "apps" keeps only the app rows; "all" keeps everything;
 * "settings" is the launcher-only scope, where the overview renders just the settings pages
 * (the expand="settings" pass does that filtering) and needs the full list.
 */
export function launchForFilter(launch: LaunchHit[], filter: Filter): LaunchHit[] {
  if (filter === "all" || filter === "settings") return launch;
  if (filter === "apps") return launch.filter((h) => h.item.kind === "app");
  return [];
}

export interface TopPick {
  launch?: LaunchHit;
  file?: Hit;
  /** Index of `file` in the name results, when it is one. */
  nameIdx?: number;
}

/**
 * The Top Hit rule (PRD §4.1): the best launcher item wins when its final score clears
 * TOP_MIN (history boosts already folded in — "picked `v` three times" clears it), then the
 * first portable exe whose stem strongly matches (the app beats its sibling `mpv.conf`),
 * then the file rule (a name result that is / starts with the query).
 */
export function pickTop(launch: LaunchHit[], names: (Hit | undefined)[], text: string, portable: Hit[] = []): TopPick {
  const top = launch[0];
  if (top && top.score >= TOP_MIN) return { launch: top };
  const p0 = portable[0];
  if (p0 && strongStem(p0.name, text)) {
    const idx = names.findIndex((n) => n === p0);
    return { file: p0, nameIdx: idx >= 0 ? idx : undefined };
  }
  const n0 = names[0];
  if (n0 && strongStem(n0.name, text)) return { file: n0, nameIdx: 0 };
  return {};
}

/** Same bar as overviewLogic's strongName (duplicated here to keep this module import-free). */
function strongStem(name: string, text: string): boolean {
  const q = text.trim().toLowerCase();
  if (!q) return false;
  const s = stemOf(name).toLowerCase();
  return s === q || s.startsWith(q) || name.toLowerCase() === q;
}

/** One launch per id inside `windowMs`: a single click already launches, so the dblclick that follows must not launch again. */
export function makeLaunchGuard(windowMs = 1500, now: () => number = Date.now) {
  let lastId = "";
  let lastAt = -Infinity;
  return (id: string): boolean => {
    const t = now();
    if (id === lastId && t - lastAt < windowMs) return false;
    lastId = id;
    lastAt = t;
    return true;
  };
}
