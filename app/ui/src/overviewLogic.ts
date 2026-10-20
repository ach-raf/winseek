import type { Hit, LaunchHit, SemanticHit } from "./types";
import { pickTop } from "./launchLogic.ts";

/** Spotlight-style result overview: a Top Hit, then a few rows per section with "Show all". */
export type SectionId = "top" | "apps" | "settings" | "commands" | "files" | "images" | "docs";
/** A section can be opened on its own ("Show all"); files open the full virtualised list instead. */
export type Expand = null | "files" | "images" | "docs" | "apps" | "settings";

export interface OvItem {
  key: string;
  section: SectionId;
  hit: Hit;
  /** The meaning / text hit behind this item (images, documents, or a top hit found by meaning). */
  sem?: SemanticHit;
  /** Index into the name results, when the item is one. */
  nameIdx?: number;
  /** Column inside the image grid. */
  col?: number;
  /** The launcher hit behind this row (apps, settings pages, system commands). */
  launch?: LaunchHit;
  /** The calculator Top Hit carries the result string (Enter copies it). */
  calc?: string;
}

export interface OvSection {
  id: SectionId;
  title: string;
  items: OvItem[];
  /** How many there are in all (name results: the searchd total). */
  total: number;
  /** More than shown: offer "Show all". */
  more: boolean;
}

export const GRID_COLS = 5;
const GRID_ROWS = 2;
const FILES_WITH_MEANING = 4;
const FILES_ALONE = 8;
const DOCS = 4;
const APPS = 4;
const SETTINGS = 4;
const COMMANDS = 3;

export const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const stem = (name: string) => { const d = name.lastIndexOf("."); return (d > 0 ? name.slice(0, d) : name).toLowerCase(); };

export const semHit = (h: SemanticHit): Hit => ({ name: baseName(h.path), path: h.path, is_dir: false, size: h.size ?? null, mtime: h.mtime ?? null });

/** A name result good enough to be the Top Hit: its name (without extension) is, or starts with, what was typed. */
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
  /** When the query is pure arithmetic, the calculator result takes the Top Hit slot. */
  calc?: string;
}): { sections: OvSection[]; flat: OvItem[] } {
  const { names, nameTotal, sem, text, expand, isImage } = opts;
  const launch = opts.launch ?? [];
  const portable = opts.portable ?? [];
  const sections: OvSection[] = [];
  const shownPaths = new Set<string>();
  const k = (p: string) => p.toLowerCase();

  // Top Hit: a pure-arithmetic query shows "= result"; else the best launcher item over
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
    sections.push({ id: "top", title: "Top Hit", items: [top], total: 1, more: false });
    shownPaths.add(k(top.hit.path));
    const resolved = top.launch ? launchPath(top.launch) : null;
    if (resolved) shownPaths.add(k(resolved)); // the same exe is not listed again under Files
  }

  // Apps: catalog apps first, then the portable exes (they open as normal files). Every app
  // row's real file is kept out of the Files section below (shownPaths).
  if (!expand || expand === "apps") {
    const catApps = launch.filter((h) => h.item.kind === "app");
    const items: OvItem[] = catApps.map((h): OvItem => ({ key: "l:" + h.item.id, section: "apps", hit: launchHit(h), launch: h }));
    if (expand === null || expand === "apps") {
      for (const h of portable) {
        const idx = names.findIndex((n) => n === h);
        items.push({ key: "p:" + h.path, section: "apps", hit: h, nameIdx: idx >= 0 ? idx : undefined });
      }
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

  if (!expand || expand === "settings") {
    const list = launch.filter((h) => h.item.kind === "setting");
    if (list.length) {
      const shown = expand ? list : list.slice(0, SETTINGS);
      sections.push({ id: "settings", title: "Settings", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "settings", hit: launchHit(h), launch: h })), total: list.length, more: !expand && list.length > shown.length });
    }
  }

  if (!expand) {
    const list = launch.filter((h) => h.item.kind === "command");
    if (list.length) {
      const shown = list.slice(0, COMMANDS);
      sections.push({ id: "commands", title: "Commands", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "commands", hit: launchHit(h), launch: h })), total: list.length, more: list.length > shown.length });
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
      if (shownPaths.has(k(h.path))) continue; // the Top Hit (file or resolved app exe) is not repeated
      items.push({ key: "n" + i, section: "files", hit: h, nameIdx: i });
      shownPaths.add(k(h.path));
    }
    const total = nameTotal - (top?.nameIdx !== undefined ? 1 : 0);
    if (items.length) sections.push({ id: "files", title: "Files", items, total, more: total > items.length });
  }

  if (!expand || expand === "images") {
    const list = imgs.filter((h) => !shownPaths.has(k(h.path)));
    const cap = expand ? list.length : GRID_COLS * GRID_ROWS;
    const items = list.slice(0, cap).map((h, i): OvItem => ({ key: "s:" + h.path, section: "images", hit: semHit(h), sem: h, col: i % GRID_COLS }));
    if (items.length) sections.push({ id: "images", title: "Images", items, total: list.length, more: list.length > items.length });
  }

  if (!expand || expand === "docs") {
    const list = docs.filter((h) => !shownPaths.has(k(h.path)));
    const cap = expand ? list.length : DOCS;
    const items = list.slice(0, cap).map((h): OvItem => ({ key: "s:" + h.path, section: "docs", hit: semHit(h), sem: h }));
    if (items.length) sections.push({ id: "docs", title: "Documents", items, total: list.length, more: list.length > items.length });
  }

  return { sections, flat: sections.flatMap((s) => s.items) };
}

/**
 * Keyboard movement through the overview. Up / down step row by row; inside the image grid they move a
 * whole grid row (leaving the grid at its edges), left / right move one tile. `jump` = next / previous section.
 */
export function ovMove(flat: OvItem[], i: number, dir: "up" | "down" | "left" | "right" | "next" | "prev"): number {
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
  if (cur.section !== "images") {
    if (dir === "left" || dir === "right") return i;
    if (dir === "down") return clamp(i + 1);
    // Up into a grid lands on the tile in the first column of its last row.
    const p = clamp(i - 1);
    if (flat[p]?.section === "images" && p !== i) {
      let s = p;
      while (s > 0 && flat[s - 1].section === "images") s--;
      return s + Math.floor((p - s) / GRID_COLS) * GRID_COLS;
    }
    return p;
  }
  let s = i;
  while (s > 0 && flat[s - 1].section === "images") s--;
  let e = i;
  while (e < flat.length - 1 && flat[e + 1].section === "images") e++;
  const pos = i - s;
  switch (dir) {
    case "left": return pos % GRID_COLS === 0 ? i : i - 1;
    case "right": return i === e || pos % GRID_COLS === GRID_COLS - 1 ? i : i + 1;
    case "down": {
      const t = i + GRID_COLS;
      if (t <= e) return t;
      // last grid row: a shorter row below exists -> its last tile; else leave the grid
      const lastRowStart = s + Math.floor((e - s) / GRID_COLS) * GRID_COLS;
      return i < lastRowStart ? e : clamp(e + 1);
    }
    case "up": return pos >= GRID_COLS ? i - GRID_COLS : clamp(s - 1);
  }
}
