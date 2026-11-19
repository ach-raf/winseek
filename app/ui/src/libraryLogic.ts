// Pure helpers for the library window (no imports, so `node --test` can load this file).

// Month names from Intl (the one month-name source in the UI); local on purpose: this file stays
// import-free for `node --test`.
const MONTHS = Array.from({ length: 12 }, (_, m) => new Intl.DateTimeFormat("en-US", { month: "long" }).format(new Date(2021, m, 15)));
const MON3 = MONTHS.map((m) => m.slice(0, 3));

// ---- timeline sections ------------------------------------------------------------------------------------

export interface Section<T> {
  /** "2025-03" */
  key: string;
  /** "March 2025" */
  label: string;
  year: number;
  /** 0-11 */
  month: number;
  items: T[];
}

/** Group by local calendar month of `mtime`: newest month first, newest item first inside each. */
export function groupByMonth<T extends { mtime: number }>(items: T[]): Section<T>[] {
  const sorted = items.slice().sort((a, b) => b.mtime - a.mtime);
  const out: Section<T>[] = [];
  for (const it of sorted) {
    const d = new Date(it.mtime);
    const year = d.getFullYear(), month = d.getMonth();
    const key = `${year}-${String(month + 1).padStart(2, "0")}`;
    const last = out[out.length - 1];
    if (last && last.key === key) last.items.push(it);
    else out.push({ key, label: `${MONTHS[month] ?? ""} ${year}`, year, month, items: [it] });
  }
  return out;
}

export interface RailMonth {
  section: number;
  label: string;
  count: number;
}
export interface RailYear {
  year: number;
  months: RailMonth[];
}

/** Year / month entries of the right-hand rail (same order as the sections). `idx` is the entry's real section index when the list was filtered. */
export function railEntries(sections: { year: number; month: number; items?: unknown[]; count?: number; idx?: number }[]): RailYear[] {
  const out: RailYear[] = [];
  sections.forEach((s, i) => {
    let y = out[out.length - 1];
    if (!y || y.year !== s.year) { y = { year: s.year, months: [] }; out.push(y); }
    y.months.push({ section: s.idx ?? i, label: MON3[s.month] ?? "", count: s.count ?? s.items?.length ?? 0 });
  });
  return out;
}

/** Index of the section the scroll position is in (the last one that starts at or above it). O(log n) over the ascending tops. */
export function activeSection(sectionTop: number[], scrollTop: number): number {
  const limit = scrollTop + 4;
  let lo = 0, hi = sectionTop.length - 1, a = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sectionTop[mid]! <= limit) { a = mid; lo = mid + 1; } else hi = mid - 1;
  }
  return a;
}

/** Scroll offset that puts section `section`'s heading at the top of the viewport (clamped to the list; 0 when unknown). */
export function jumpOffset(sectionTop: number[], section: number): number {
  const i = Math.max(0, Math.min(sectionTop.length - 1, section));
  return sectionTop[i] ?? 0;
}

/** Section index of a month key ("2026-11"), -1 when the timeline has no such section. */
export function sectionOfKey(sections: { key: string }[], key: string): number {
  return sections.findIndex((s) => s.key === key);
}

/** One rail month as a vertical slot in the rail's content coordinates. */
export interface RailSlot {
  top: number;
  bottom: number;
  count: number;
}

/**
 * The slot a pointer at rail y lands on while scrubbing: the slot under it (or the nearest one in the gaps and past
 * the ends), and when that month is empty the nearest month with photos, preferring the direction of travel
 * (`prev` is the slot the scrub was on). -1 when no month has photos.
 */
export function scrubSlot(slots: RailSlot[], y: number, prev = -1): number {
  const n = slots.length;
  if (n === 0) return -1;
  // Binary search: the last slot starting at or above y.
  let lo = 0, hi = n - 1, at = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (slots[mid]!.top <= y) { at = mid; lo = mid + 1; } else hi = mid - 1;
  }
  // In the gap after `at` (a year heading): the nearer of the two neighbours.
  if (y > slots[at]!.bottom && at + 1 < n && slots[at + 1]!.top - y < y - slots[at]!.bottom) at++;
  if (slots[at]!.count > 0) return at;
  const fwd = prev < 0 || at >= prev; // moving down (or the first move)
  for (let d = 1; d < n; d++) {
    const a = fwd ? at + d : at - d, b = fwd ? at - d : at + d;
    if (a >= 0 && a < n && slots[a]!.count > 0) return a;
    if (b >= 0 && b < n && slots[b]!.count > 0) return b;
  }
  return -1;
}

/** The next month with photos before / after `from` (the `[` / `]` keys); `from` itself when there is none. */
export function stepMonth(counts: number[], from: number, dir: 1 | -1): number {
  for (let i = from + dir; i >= 0 && i < counts.length; i += dir) if ((counts[i] ?? 0) > 0) return i;
  return from;
}

// ---- grid layout + windowing ------------------------------------------------------------------------------

export interface GridRow {
  kind: "head" | "cells";
  section: number;
  top: number;
  h: number;
  label?: string;
  /** Flat item range [start, end) for cell rows. */
  start: number;
  end: number;
}
export interface GridLayout {
  rows: GridRow[];
  total: number;
  /** Top offset of each section (its heading, or its first row when it has none). */
  sectionTop: number[];
  cols: number;
  cell: number;
  gap: number;
}

export const GRID_PAD = 20;

/** Columns and square cell size for a given width: cells grow to fill the row, never below `minCell`. */
export function gridCols(width: number, minCell: number, gap: number, pad = GRID_PAD): { cols: number; cell: number } {
  const inner = Math.max(0, width - pad * 2);
  const cols = Math.max(1, Math.floor((inner + gap) / (minCell + gap)));
  const cell = Math.max(32, Math.floor((inner - gap * (cols - 1)) / cols));
  return { cols, cell };
}

/** Fixed-size rows: an optional heading row per section, then rows of `cols` square cells. */
export function layoutGrid(sizes: number[], labels: (string | null)[], cols: number, cell: number, gap: number, headH: number): GridLayout {
  const rows: GridRow[] = [];
  const sectionTop: number[] = [];
  let top = 8, flat = 0;
  sizes.forEach((n, s) => {
    sectionTop.push(top);
    if (n === 0) return;
    const label = labels[s];
    if (label) {
      rows.push({ kind: "head", section: s, top, h: headH, label, start: flat, end: flat });
      top += headH;
    }
    for (let k = 0; k < n; k += cols) {
      rows.push({ kind: "cells", section: s, top, h: cell + gap, start: flat + k, end: flat + Math.min(k + cols, n) });
      top += cell + gap;
    }
    flat += n;
  });
  return { rows, total: top + GRID_PAD, sectionTop, cols, cell, gap };
}

/** Rows [from, to) that intersect the viewport (plus `overscan` pixels each side). */
export function visibleRows(rows: { top: number; h: number }[], scrollTop: number, height: number, overscan = 240): [number, number] {
  const lo = scrollTop - overscan, hi = scrollTop + height + overscan;
  let a = 0, b = rows.length;
  while (a < b) {
    const m = (a + b) >> 1;
    const row = rows[m]!; // m sits in [a, b) inside rows
    if (row.top + row.h <= lo) a = m + 1; else b = m;
  }
  let to = a;
  while (to < rows.length && rows[to]!.top < hi) to++;
  return [a, to];
}

/** Fixed-height list: item range [from, to) in view. */
export function uniformRange(n: number, rowH: number, scrollTop: number, height: number, overscan = 4): [number, number] {
  const from = Math.max(0, Math.floor(scrollTop / rowH) - overscan);
  const to = Math.min(n, Math.ceil((scrollTop + height) / rowH) + overscan);
  return [from, Math.max(from, to)];
}

/** The cell row that holds flat item `idx`, or null. */
export function rowOfItem(layout: GridLayout, idx: number): GridRow | null {
  for (const r of layout.rows) if (r.kind === "cells" && idx >= r.start && idx < r.end) return r;
  return null;
}

/** New scrollTop that brings `row` fully into view (unchanged when it already is). */
export function scrollIntoView(row: { top: number; h: number }, scrollTop: number, height: number, margin = 8): number {
  if (row.top - margin < scrollTop) return Math.max(0, row.top - margin);
  if (row.top + row.h + margin > scrollTop + height) return row.top + row.h + margin - height;
  return scrollTop;
}

/** Arrow-key navigation over `n` items laid out `cols` per row. */
export function moveIndex(i: number, key: string, cols: number, n: number): number {
  if (n <= 0) return 0;
  switch (key) {
    case "ArrowLeft": return Math.max(0, i - 1);
    case "ArrowRight": return Math.min(n - 1, i + 1);
    case "ArrowUp": return i - cols >= 0 ? i - cols : i;
    case "ArrowDown": return i + cols <= n - 1 ? i + cols : i < n - 1 ? n - 1 : i;
    case "Home": return 0;
    case "End": return n - 1;
    default: return i;
  }
}

// ---- smart collections ------------------------------------------------------------------------------------

/** The "Screenshots" name rule. */
export function isScreenshot(name: string): boolean {
  return /screen\s?shot|screen\s?capture|^snip|^capture[ _-]?\d|^bildschirmfoto|^capture d.cran/i.test(name);
}

// ---- clean-up ---------------------------------------------------------------------------------------------

export interface KeepCandidate {
  path: string;
  mtime: number;
  width?: number | null;
  height?: number | null;
}

/** Index of the copy to keep: most pixels, then newest, then shortest path (then alphabetical). */
export function pickKeeper(members: KeepCandidate[]): number {
  let best = 0;
  const px = (m: KeepCandidate) => (m.width ?? 0) * (m.height ?? 0);
  for (let i = 1; i < members.length; i++) {
    const a = members[i]!, b = members[best]!; // both indices stay inside members
    const d = px(a) - px(b) || a.mtime - b.mtime || b.path.length - a.path.length || (a.path < b.path ? 1 : a.path > b.path ? -1 : 0);
    if (d > 0) best = i;
  }
  return best;
}

/** Bytes freed by deleting every member except `keep`. */
export function freedBytes(members: { size: number }[], keep: number): number {
  return members.reduce((a, m, i) => a + (i === keep ? 0 : m.size), 0);
}

/** Bytes freed by keeping only the largest of each group. */
export function totalReclaimable(groups: { reclaimable: number }[]): number {
  return groups.reduce((a, g) => a + g.reclaimable, 0);
}

// ---- timeline views ---------------------------------------------------------------------------------------

export type TimelineView = "all" | "photos" | "screenshots" | "graphics";
type ImgClassName = "photo" | "screenshot" | "graphic";

/** "all" is everything of yours: photos and screenshots. App graphics only show in their own view. */
export const TIMELINE_VIEWS: { id: TimelineView; label: string; hint: string }[] = [
  { id: "all", label: "All", hint: "Your photos and screenshots" },
  { id: "photos", label: "Photos", hint: "Camera pictures and saved images" },
  { id: "screenshots", label: "Screenshots", hint: "Screen captures" },
  { id: "graphics", label: "App graphics", hint: "Icons, logos and other images that came with apps and games. Hidden everywhere else." },
];

export function parseTimelineView(s: string | null | undefined): TimelineView {
  return TIMELINE_VIEWS.some((v) => v.id === s) ? (s as TimelineView) : "all";
}

export function inView(cls: ImgClassName, view: TimelineView): boolean {
  switch (view) {
    case "all": return cls !== "graphic";
    case "photos": return cls === "photo";
    case "screenshots": return cls === "screenshot";
    case "graphics": return cls === "graphic";
  }
}

export function countViews(items: { class: ImgClassName }[]): Record<TimelineView, number> {
  const out: Record<TimelineView, number> = { all: 0, photos: 0, screenshots: 0, graphics: 0 };
  for (const i of items) {
    if (i.class === "graphic") out.graphics++;
    else { out.all++; if (i.class === "photo") out.photos++; else out.screenshots++; }
  }
  return out;
}

// ---- collections: ready-made ideas ------------------------------------------------------------------------

export interface CollectionIdea {
  name: string;
  /** Several phrasings, comma-separated: each one is searched on its own. */
  query: string;
}

/** Descriptions that work well with the search model: concrete things one can see, a few phrasings each. */
export const COLLECTION_IDEAS: CollectionIdea[] = [
  { name: "Medical", query: "a medical document, a doctor's prescription, a lab test result, a vaccination certificate, an x-ray or medical scan" },
  { name: "IDs and passports", query: "an identity card, a passport page, a driving licence, a residence permit" },
  { name: "Bills and invoices", query: "an invoice, a utility bill, a paper receipt, a bank statement" },
  { name: "Tickets and bookings", query: "a boarding pass, a train or concert ticket, a hotel booking confirmation" },
  { name: "Handwritten notes", query: "a handwritten note, a page of handwriting, a whiteboard with notes" },
  { name: "Maps and directions", query: "a map, a screenshot of a navigation app, a floor plan" },
  { name: "Charts and tables", query: "a chart or graph, a spreadsheet table, a dashboard with statistics" },
  { name: "Chats", query: "a screenshot of a chat conversation, a text message thread" },
  { name: "Pets", query: "a photo of a dog, a photo of a cat, a pet animal at home" },
  { name: "Food", query: "a photo of a meal, a plate of food, a restaurant menu" },
];

/** Ideas whose name is not taken yet. */
export function freshIdeas(taken: string[]): CollectionIdea[] {
  const have = new Set(taken.map((n) => n.trim().toLowerCase()));
  return COLLECTION_IDEAS.filter((i) => !have.has(i.name.toLowerCase()));
}

// ---- clean-up: old screenshots ----------------------------------------------------------------------------

export const OLD_SHOT_DAYS = 180;

/** Screenshots not touched for `days`, biggest first. */
export function oldScreenshots<T extends { class: ImgClassName; mtime: number; size: number }>(items: T[], now: number, days = OLD_SHOT_DAYS): T[] {
  const before = now - days * 24 * 3600 * 1000;
  return items.filter((i) => i.class === "screenshot" && i.mtime < before).sort((a, b) => b.size - a.size);
}

// ---- gallery view + list filters (the grid/list toggle and the name filter fields) --------------------------

/** How a gallery shows its items. */
export type GalleryView = "grid" | "list";

/** Anything unexpected (old value, missing, junk) means the grid. */
export function parseGalleryView(v: unknown): GalleryView {
  return v === "list" ? "list" : "grid";
}

/** Case-insensitive substring match for the per-list filter fields; an empty query matches everything. */
export function nameMatches(name: string, query: string): boolean {
  const q = query.trim().toLowerCase();
  return q.length === 0 || name.toLowerCase().includes(q);
}

/** Fold keys read back from storage (a JSON array of strings); anything else means no folds. */
export function parseFoldSet(v: unknown): Set<string> {
  if (!Array.isArray(v)) return new Set();
  return new Set(v.filter((x): x is string => typeof x === "string"));
}

// ---- multi-select (Ctrl+click toggle, Shift+click range) ----------------------------------------------------

/** What a click meant: the held modifiers plus, for Shift, the order the rows are shown in right now. */
export interface SelMod {
  ctrl?: boolean;
  shift?: boolean;
  /** Context-menu click: an already-selected item keeps the whole batch (Explorer behaviour). */
  ctx?: boolean;
  /** The visible order of keys (paths) in the surface that was clicked, for Shift ranges. */
  order?: readonly string[];
}

/** Is `path` one of the batch keys? Paths compare case-insensitively (Windows paths). */
export function inBatch(batch: ReadonlySet<string> | null, path: string, lead?: string | null): boolean {
  const p = path.toLowerCase();
  if (batch) { for (const x of batch) if (x.toLowerCase() === p) return true; return false; }
  return lead !== undefined && lead !== null && lead.toLowerCase() === p;
}

/** Ctrl+click: add or remove one key from the batch (membership ignores case; the stored paths keep
 *  their real case, they are what a batch action hands to the backend). Toggling off may empty the
 *  batch — the caller then falls back to the single-lead selection. */
export function toggledBatch(cur: ReadonlySet<string>, key: string): string[] {
  const k = key.toLowerCase();
  const out: string[] = [];
  let had = false;
  for (const x of cur) { if (x.toLowerCase() === k) had = true; else out.push(x); }
  if (!had) out.push(key);
  return out;
}

/** Shift+click: everything between the anchor and the clicked key in the order shown now;
 *  an anchor that is no longer on screen (or none yet) means just the clicked key. */
export function rangedBatch(order: readonly string[], anchor: string | null, key: string): string[] {
  const b = order.indexOf(key);
  const a = anchor ? order.findIndex((x) => x.toLowerCase() === anchor.toLowerCase()) : -1;
  if (a < 0 || b < 0 || a === b) return [key];
  const [lo, hi] = a < b ? [a, b] : [b, a];
  return order.slice(lo, hi + 1);
}
