import type { Hit, LaunchHit, LaunchItem, SemanticHit } from "./types";
import { pickTop } from "./launchLogic.ts";

/** Result overview: a Top result, then a few rows per section with "Show all"; home = Most used + Recent files. */
export type SectionId = "top" | "apps" | "settings" | "commands" | "files" | "images" | "docs" | "mostused" | "recent";
/** A section can be opened on its own ("Show all"); files open the full virtualised list instead. */
export type Expand = null | "files" | "images" | "docs" | "apps" | "settings";

/** The section Alt+Right can expand for this item; null when there is no "Show all"
 *  (the Top result and the system commands never have one). */
export function expandable(item: OvItem | undefined): Expand {
  switch (item?.section) {
    case "files": case "images": case "docs": case "apps": case "settings": return item.section;
    default: return null;
  }
}

export interface OvItem {
  key: string;
  section: SectionId;
  hit: Hit;
  /** The meaning / text hit behind this item (photos, related documents, or a top result found by meaning). */
  sem?: SemanticHit;
  /** Index into the name results, when the item is one. */
  nameIdx?: number;
  /** Column inside the photo grid. */
  col?: number;
  /** The launcher hit behind this row (apps, settings pages, system commands). */
  launch?: LaunchHit;
  /** The calculator Top result carries the result string (Enter copies it). */
  calc?: string;
}

export interface OvSection {
  id: SectionId;
  title: string;
  /** Right-aligned note in the section header ("Found by what the file shows or says, not its name"). */
  note?: string;
  items: OvItem[];
  /** How many there are in all (name results: the searchd total). */
  total: number;
  /** More than shown: offer "Show all". */
  more: boolean;
}

/** Photo grid columns in the compact popup (the full window shows 5, the app grid 6 / 8). */
export const GRID_COLS = 4;
const GRID_ROWS = 2;
/** Sections drawn as a tile grid: arrow keys walk them by column. */
export type GridCols = Partial<Record<SectionId, number>>;
export const DEFAULT_COLS: GridCols = { images: GRID_COLS, mostused: 6 };
export const isGrid = (s: SectionId | undefined): boolean => s === "images" || s === "mostused";

const FILES_WITH_MEANING = 4;
const FILES_ALONE = 8;
const DOCS = 4;
const APPS = 4;
const SETTINGS = 4;
const COMMANDS = 3;

export const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const stem = (name: string) => { const d = name.lastIndexOf("."); return (d > 0 ? name.slice(0, d) : name).toLowerCase(); };

export const semHit = (h: SemanticHit): Hit => ({ name: baseName(h.path), path: h.path, is_dir: false, size: h.size ?? null, mtime: h.mtime ?? null });

/** A name result good enough to be the Top result: its name (without extension) is, or starts with, what was typed. */
export function strongName(name: string, text: string): boolean {
  const q = text.trim().toLowerCase();
  if (!q) return false;
  const s = stem(name);
  return s === q || s.startsWith(q) || name.toLowerCase() === q;
}

/** A display Hit for a launch row (icon / title rendering shares the file-row plumbing). */
export function launchHit(h: LaunchHit): Hit {
  const t = h.item.target;
  const path = t.t === "path" ? t.path : t.t === "aumid" ? (t.path ?? `shell:AppsFolder\\${t.aumid}`) : t.t === "uri" ? t.uri : t.t === "exec" ? t.exe : `winseek:${t.name}`;
  return { name: h.item.title, path, is_dir: false, size: null, mtime: null };
}

/** The target path a launch item resolves to (dedupes it against file rows). */
export const launchPath = (h: LaunchHit): string | null => {
  const t = h.item.target;
  return t.t === "path" ? t.path : t.t === "aumid" ? t.path : null;
};

export function buildOverview(opts: {
  names: (Hit | undefined)[]; nameTotal: number; sem: SemanticHit[]; text: string; expand: Expand;
  /** File name -> is a picture (format.kindOf; passed in so this module has no imports). */
  isImage: (name: string) => boolean;
  /** Launcher hits (apps, settings, commands), best first. */
  launch?: LaunchHit[];
  /** Portable exes promoted from the name index (PRD R9). */
  portable?: Hit[];
  /** When the query is pure arithmetic, the calculator result takes the Top result slot. */
  calc?: string;
  /** Columns of the Photos grid (4 in the popup, 5 in the full window); two rows are shown. */
  photoCols?: number;
}): { sections: OvSection[]; flat: OvItem[] } {
  const { names, nameTotal, sem, text, expand, isImage } = opts;
  const launch = opts.launch ?? [];
  const portable = opts.portable ?? [];
  const photoCols = Math.max(1, opts.photoCols ?? GRID_COLS);
  const sections: OvSection[] = [];
  const shownPaths = new Set<string>();
  const k = (p: string) => p.toLowerCase();

  // Top result: a pure-arithmetic query shows "= result"; else the best launcher item over
  // TOP_MIN (history folded into its score), else an obvious name match, else the best
  // meaning hit when nothing matches by name.
  let top: OvItem | null = null;
  if (!expand && opts.calc !== undefined) {
    const item: LaunchHit = {
      item: { id: "calc", kind: "command", title: `= ${opts.calc}`, subtitle: "Press Enter to copy", keywords: [], target: { t: "cmd", name: "calc" }, icon: null, admin_ok: false },
      score: 0, ranges: [], confirm: false,
    };
    top = { key: "calc", section: "top", hit: { name: `= ${opts.calc}`, path: "winseek:calc", is_dir: false, size: null, mtime: null }, launch: item, calc: opts.calc };
  } else {
    const picked = pickTop(launch, names, text, portable);
    if (!expand) {
      if (picked.launch) {
        top = { key: "l:" + picked.launch.item.id, section: "top", hit: launchHit(picked.launch), launch: picked.launch };
      } else if (picked.file) {
        top = { key: "n0", section: "top", hit: picked.file, nameIdx: picked.nameIdx };
      } else if (names[0] === undefined && nameTotal === 0 && sem.length) {
        top = { key: "s:" + sem[0].path, section: "top", hit: semHit(sem[0]), sem: sem[0] };
      }
    }
  }
  if (top) {
    sections.push({ id: "top", title: "Top result", items: [top], total: 1, more: false });
    shownPaths.add(k(top.hit.path));
    const resolved = top.launch ? launchPath(top.launch) : null;
    if (resolved) shownPaths.add(k(resolved)); // the same exe is not listed again under Files
  }
  const notTop = (h: LaunchHit) => top?.key !== "l:" + h.item.id; // the top result is taken out of its own section

  // Apps: catalog apps first, then the portable exes (they open as normal files). Every app
  // row's real file is kept out of the Files section below (shownPaths).
  if (!expand || expand === "apps") {
    const catApps = launch.filter((h) => h.item.kind === "app" && notTop(h));
    const items: OvItem[] = catApps.map((h): OvItem => ({ key: "l:" + h.item.id, section: "apps", hit: launchHit(h), launch: h }));
    for (const h of portable) {
      if (top && !top.launch && k(h.path) === k(top.hit.path)) continue;
      const idx = names.findIndex((n) => n === h);
      items.push({ key: "p:" + h.path, section: "apps", hit: h, nameIdx: idx >= 0 ? idx : undefined });
    }
    const total = items.length;
    if (items.length) {
      const shown = expand ? items : items.slice(0, APPS);
      for (const it of shown) {
        shownPaths.add(k(it.hit.path));
        if (it.launch) {
          const resolved = launchPath(it.launch);
          if (resolved) shownPaths.add(k(resolved));
        }
      }
      sections.push({ id: "apps", title: "Apps", items: shown, total, more: !expand && total > shown.length });
    }
  }

  const rest = sem.filter((h) => !shownPaths.has(k(h.path)));
  const imgs = rest.filter((h) => isImage(baseName(h.path)));
  const docs = rest.filter((h) => !isImage(baseName(h.path)));
  const hasMeaning = imgs.length + docs.length > 0;

  if (!expand) {
    const cap = hasMeaning ? FILES_WITH_MEANING : FILES_ALONE;
    const items: OvItem[] = [];
    for (let i = 0; i < names.length && items.length < cap; i++) {
      const h = names[i];
      if (!h) break;
      if (shownPaths.has(k(h.path))) continue; // the Top result (file or resolved app exe) is not repeated
      items.push({ key: "n" + i, section: "files", hit: h, nameIdx: i });
      shownPaths.add(k(h.path));
    }
    const total = nameTotal - (top?.nameIdx !== undefined ? 1 : 0);
    if (items.length) sections.push({ id: "files", title: "Files", items, total, more: total > items.length });
  }

  if (!expand || expand === "images") {
    const list = imgs.filter((h) => !shownPaths.has(k(h.path)));
    const cap = expand ? list.length : photoCols * GRID_ROWS;
    const items = list.slice(0, cap).map((h, i): OvItem => ({ key: "s:" + h.path, section: "images", hit: semHit(h), sem: h, col: i % photoCols }));
    if (items.length) sections.push({ id: "images", title: "Photos", items, total: list.length, more: list.length > items.length });
  }

  if (!expand || expand === "docs") {
    const list = docs.filter((h) => !shownPaths.has(k(h.path)));
    const cap = expand ? list.length : DOCS;
    const items = list.slice(0, cap).map((h): OvItem => ({ key: "s:" + h.path, section: "docs", hit: semHit(h), sem: h }));
    if (items.length) {
      sections.push({ id: "docs", title: "Related by meaning", note: "Found by what the file shows or says, not its name", items, total: list.length, more: list.length > items.length });
    }
  }

  if (!expand || expand === "settings") {
    const list = launch.filter((h) => h.item.kind === "setting" && notTop(h));
    if (list.length) {
      const shown = expand ? list : list.slice(0, SETTINGS);
      sections.push({ id: "settings", title: "Windows settings", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "settings", hit: launchHit(h), launch: h })), total: list.length, more: !expand && list.length > shown.length });
    }
  }

  if (!expand) {
    const list = launch.filter((h) => h.item.kind === "command" && notTop(h));
    if (list.length) {
      const shown = list.slice(0, COMMANDS);
      sections.push({ id: "commands", title: "Commands", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "commands", hit: launchHit(h), launch: h })), total: list.length, more: list.length > shown.length });
    }
  }

  return { sections, flat: sections.flatMap((s) => s.items) };
}

// ---- home: "Most used" apps (a stored launch count) + "Recent files" ----

export interface MruEntry { item: LaunchItem; n: number; t: number }
export const MRU_MAX = 12;

/** Count one more launch of `item`; most launched first (ties: most recent first), at most MRU_MAX. */
export function bumpMru(list: MruEntry[], item: LaunchItem, now: number): MruEntry[] {
  const cur = list.find((e) => e.item.id === item.id);
  const next = cur ? list.map((e) => (e === cur ? { item, n: e.n + 1, t: now } : e)) : [...list, { item, n: 1, t: now }];
  return next.sort((a, b) => b.n - a.n || b.t - a.t).slice(0, MRU_MAX);
}

/** Home (empty query): "Most used" apps as a tile grid, then "Recent files" rows. */
export function buildHome(opts: {
  mru: LaunchHit[]; recent: (Hit | undefined)[]; appCols: number; recentCap: number;
}): { sections: OvSection[]; flat: OvItem[] } {
  const sections: OvSection[] = [];
  const apps = opts.mru.slice(0, Math.max(1, opts.appCols));
  if (apps.length) {
    sections.push({
      id: "mostused", title: "Most used", total: apps.length, more: false,
      items: apps.map((h): OvItem => ({ key: "l:" + h.item.id, section: "mostused", hit: launchHit(h), launch: h })),
    });
  }
  const files: OvItem[] = [];
  for (let i = 0; i < opts.recent.length && files.length < opts.recentCap; i++) {
    const h = opts.recent[i];
    if (h) files.push({ key: "n" + i, section: "recent", hit: h, nameIdx: i });
  }
  if (files.length) sections.push({ id: "recent", title: "Recent files", items: files, total: files.length, more: false });
  return { sections, flat: sections.flatMap((x) => x.items) };
}

// ---- display helpers (pure) ----

/** The folder a file is in, home folder dropped, parts joined with " › ": head shrinks with an ellipsis, tail stays.
 *  "C:\Users\Me\Documents\Finance\x.pdf" -> head "Documents", tail " › Finance". */
export function nicePath(path: string): { head: string; tail: string } {
  const i = path.lastIndexOf("\\");
  const dir = i > 0 ? path.slice(0, i) : path;
  const home = /^[A-Za-z]:\\Users\\[^\\]+/i;
  let parts: string[];
  if (home.test(dir)) {
    parts = dir.replace(home, "").split("\\").filter(Boolean);
    if (!parts.length) parts = ["Home"];
  } else parts = dir.split("\\").filter(Boolean);
  if (!parts.length) return { head: "", tail: dir };
  const last = parts[parts.length - 1];
  return parts.length === 1 ? { head: "", tail: last } : { head: parts.slice(0, -1).join(" \u203a "), tail: " \u203a " + last };
}

/** "Today", "Yesterday", "3 days ago", "2 weeks ago", "5 months ago", "2 years ago" (calendar days). */
export function relDate(ms: number | null, now = Date.now()): string {
  if (ms === null) return "";
  const day = (t: number) => { const d = new Date(t); return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime(); };
  const d = Math.round((day(now) - day(ms)) / 86400000);
  if (d <= 0) return "Today";
  if (d === 1) return "Yesterday";
  if (d < 7) return `${d} days ago`;
  if (d < 60) return `${Math.round(d / 7)} weeks ago`;
  if (d < 700) return `${Math.round(d / 30)} months ago`;
  return `${Math.round(d / 365)} years ago`;
}

/** Search time for the stat: one decimal below 10 ms ("0.4"), whole milliseconds above. */
export const fmtMs = (ms: number): string => (ms < 10 ? Math.max(0.1, ms).toFixed(1) : String(Math.round(ms)));

/** The drive scope as a Tier 0 path term: `C:\` matches paths that start there (a term with a backslash is a path match). */
export const scopeDrive = (q: string, drive: string): string => (drive ? `${q} ${drive}:\\`.trim() : q);
/** Client-side twin of scopeDrive, for lists the name index did not answer (recent files, meaning hits). */
export const onDrive = (path: string, drive: string): boolean => !drive || path.toLowerCase().startsWith(drive.toLowerCase() + ":");

/**
 * Keyboard movement through the overview. Up / down step row by row; inside a tile grid (Photos, Most used)
 * they move a whole grid row (leaving the grid at its edges), left / right move one tile. `next` / `prev` jump
 * a section. `cols` = the columns of each grid section as currently laid out.
 */
export function ovMove(flat: OvItem[], i: number, dir: "up" | "down" | "left" | "right" | "next" | "prev", cols: GridCols = DEFAULT_COLS): number {
  if (!flat.length) return 0;
  const cur = flat[Math.max(0, Math.min(i, flat.length - 1))];
  const clamp = (n: number) => Math.max(0, Math.min(flat.length - 1, n));
  if (dir === "next" || dir === "prev") {
    if (dir === "next") { const j = flat.findIndex((x, n) => n > i && x.section !== cur.section); return j < 0 ? i : j; }
    let j = i;
    while (j > 0 && flat[j - 1].section === cur.section) j--; // start of this section
    if (j !== i) return j;
    if (j === 0) return 0;
    const prevSec = flat[j - 1].section;
    while (j > 0 && flat[j - 1].section === prevSec) j--;
    return j;
  }
  const colsOf = (sec: SectionId) => Math.max(1, cols[sec] ?? 1);
  if (!isGrid(cur.section)) {
    if (dir === "left" || dir === "right") return i;
    if (dir === "down") return clamp(i + 1);
    // Up into a grid lands on the tile in the first column of its last row.
    const p = clamp(i - 1);
    const sec = flat[p]?.section;
    if (sec && isGrid(sec) && p !== i) {
      let s = p;
      while (s > 0 && flat[s - 1].section === sec) s--;
      return s + Math.floor((p - s) / colsOf(sec)) * colsOf(sec);
    }
    return p;
  }
  const sec = cur.section, n = colsOf(sec);
  let s = i;
  while (s > 0 && flat[s - 1].section === sec) s--;
  let e = i;
  while (e < flat.length - 1 && flat[e + 1].section === sec) e++;
  const pos = i - s;
  switch (dir) {
    case "left": return pos % n === 0 ? i : i - 1;
    case "right": return i === e || pos % n === n - 1 ? i : i + 1;
    case "down": {
      const t = i + n;
      if (t <= e) return t;
      // last grid row: a shorter row below exists -> its last tile; else leave the grid
      const lastRowStart = s + Math.floor((e - s) / n) * n;
      return i < lastRowStart ? e : clamp(e + 1);
    }
    case "up": return pos >= n ? i - n : clamp(s - 1);
  }
}
