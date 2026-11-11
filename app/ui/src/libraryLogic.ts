// Pure helpers for the library window (no imports, so `node --test` can load this file).

const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
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
    else out.push({ key, label: `${MONTHS[month]} ${year}`, year, month, items: [it] });
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

/** Year / month entries of the right-hand rail (same order as the sections). */
export function railEntries(sections: { year: number; month: number; items: unknown[] }[]): RailYear[] {
  const out: RailYear[] = [];
  sections.forEach((s, i) => {
    let y = out[out.length - 1];
    if (!y || y.year !== s.year) { y = { year: s.year, months: [] }; out.push(y); }
    y.months.push({ section: i, label: MON3[s.month], count: s.items.length });
  });
  return out;
}

/** Index of the section the scroll position is in (the last one that starts at or above it). */
export function activeSection(sectionTop: number[], scrollTop: number): number {
  let a = 0;
  for (let i = 0; i < sectionTop.length; i++) if (sectionTop[i] <= scrollTop + 4) a = i;
  return a;
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
    if (rows[m].top + rows[m].h <= lo) a = m + 1; else b = m;
  }
  let to = a;
  while (to < rows.length && rows[to].top < hi) to++;
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

// ---- v4 mosaic (timeline) ---------------------------------------------------------------------------------

export interface MosaicTile {
  /** Flat item index. */
  idx: number;
  section: number;
  col: number;
  /** Row inside the section's mosaic. */
  row: number;
  w: number;
  h: number;
}
export interface MosaicSection {
  /** Flat range [first, end). */
  first: number;
  end: number;
  rows: number;
}
export interface Mosaic {
  tiles: MosaicTile[];
  sections: MosaicSection[];
  cols: number;
}
export interface MosaicMetrics {
  top: number[];
  h: number[];
  head: number[];
  total: number;
}

/** Feature tiles of the prototype: the first photo of a month is 2x2, every 5th is 2x1, plus a 2x2 now and then. */
export function tileSpan(j: number, cols: number): [number, number] {
  if (cols < 4) return [1, 1];
  if (j === 0 || j % 11 === 6) return [2, 2];
  if (j % 5 === 4) return [2, 1];
  return [1, 1];
}

/** Packs every section like `grid-auto-flow: dense` (each tile takes the first free slot), independent of pixel sizes. */
export function packMosaic(sizes: number[], cols: number): Mosaic {
  const tiles: MosaicTile[] = [];
  const sections: MosaicSection[] = [];
  let flat = 0;
  sizes.forEach((n, s) => {
    const occ: Uint8Array[] = [];
    const rowOf = (r: number) => (occ[r] ??= new Uint8Array(cols));
    let minRow = 0, rows = 0;
    for (let j = 0; j < n; j++) {
      const [w0, h] = tileSpan(j, cols);
      const w = Math.min(w0, cols);
      let placed = false;
      for (let r = minRow; !placed; r++) {
        for (let c = 0; c + w <= cols && !placed; c++) {
          let ok = true;
          for (let dr = 0; dr < h && ok; dr++) for (let dc = 0; dc < w; dc++) if (rowOf(r + dr)[c + dc]) { ok = false; break; }
          if (!ok) continue;
          for (let dr = 0; dr < h; dr++) for (let dc = 0; dc < w; dc++) rowOf(r + dr)[c + dc] = 1;
          tiles.push({ idx: flat + j, section: s, col: c, row: r, w, h });
          rows = Math.max(rows, r + h);
          placed = true;
        }
      }
      while (occ[minRow] && occ[minRow].every(Boolean)) minRow++;
    }
    sections.push({ first: flat, end: flat + n, rows });
    flat += n;
  });
  return { tiles, sections, cols };
}

/** Vertical metrics for a packed mosaic: `headH[s]` is the heading height of section s (0 = none). */
export function mosaicMetrics(m: Mosaic, rowH: number, gap: number, headH: number[], bottom: number, pad: number): MosaicMetrics {
  const top: number[] = [], h: number[] = [];
  let y = 0;
  m.sections.forEach((s, i) => {
    const body = s.rows > 0 ? s.rows * (rowH + gap) - gap : 0;
    const sh = headH[i] + body + (s.rows > 0 ? bottom : 0);
    top.push(y);
    h.push(sh);
    y += sh;
  });
  return { top, h, head: headH, total: y + pad };
}

/** Section-relative top and pixel height of a tile. */
export function tileBox(t: MosaicTile, rowH: number, gap: number, head: number): { y: number; h: number } {
  return { y: head + t.row * (rowH + gap), h: t.h * rowH + (t.h - 1) * gap };
}

/** Arrow-key move over a mosaic: left/right follow the order, up/down go to the nearest tile above/below. */
export function moveMosaic(m: Mosaic, mt: MosaicMetrics, rowH: number, gap: number, i: number, key: string, n: number): number {
  if (n <= 0) return 0;
  switch (key) {
    case "ArrowLeft": return Math.max(0, i - 1);
    case "ArrowRight": return Math.min(n - 1, i + 1);
    case "Home": return 0;
    case "End": return n - 1;
  }
  if (key !== "ArrowUp" && key !== "ArrowDown") return i;
  const cur = m.tiles[i];
  if (!cur) return i;
  const gy = (t: MosaicTile) => { const b = tileBox(t, rowH, gap, mt.head[t.section]); return { y: mt.top[t.section] + b.y, h: b.h }; };
  const c = gy(cur), cx = cur.col + cur.w / 2;
  let best = -1, bestDy = Infinity, bestDx = Infinity;
  for (let k = Math.max(0, i - 64); k <= Math.min(n - 1, i + 64); k++) {
    if (k === i) continue;
    const t = m.tiles[k], b = gy(t);
    const dy = key === "ArrowDown" ? b.y - (c.y + c.h) : c.y - (b.y + b.h);
    if (dy < -1) continue;
    const dx = Math.abs(t.col + t.w / 2 - cx);
    if (dy < bestDy - 1 || (Math.abs(dy - bestDy) <= 1 && dx < bestDx)) { best = k; bestDy = dy; bestDx = dx; }
  }
  return best >= 0 ? best : i;
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
    const a = members[i], b = members[best];
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
