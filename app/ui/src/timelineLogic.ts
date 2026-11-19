// The timeline without the photo list: month sections known by count, rows fetched by absolute index in pages,
// the mosaic laid out from one shared template, selections kept as index ranges. Pure (no imports), so `node --test` can load it.

// ---- sections: absolute vs effective indices -----------------------------------------------------------------
// "Absolute" counts every photo of the filtered list (what the backend's range fetch speaks).
// "Effective" skips the photos of folded months: what the screen shows, what the arrow keys walk.

export interface SecIndex {
  count: number[];
  absStart: number[];
  effCount: number[];
  effStart: number[];
  absTotal: number;
  effTotal: number;
}

export function buildIndex(secs: readonly { key: string; count: number }[], folded: ReadonlySet<string>): SecIndex {
  const count: number[] = [], absStart: number[] = [], effCount: number[] = [], effStart: number[] = [];
  let a = 0, e = 0;
  for (const s of secs) {
    const ec = folded.has(s.key) ? 0 : s.count;
    count.push(s.count); absStart.push(a); effCount.push(ec); effStart.push(e);
    a += s.count; e += ec;
  }
  return { count, absStart, effCount, effStart, absTotal: a, effTotal: e };
}

/** Last index in the ascending `starts` that is <= v (-1 when none). */
function lastAtOrBelow(starts: readonly number[], v: number): number {
  let lo = 0, hi = starts.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (starts[m]! <= v) lo = m + 1; else hi = m;
  }
  return lo - 1;
}

/** The section that holds effective index `e` (folded sections hold none, so the answer is never one of them). */
export function sectionOfEff(ix: SecIndex, e: number): number {
  return Math.max(0, lastAtOrBelow(ix.effStart, e));
}
/** The section that holds absolute index `a`. */
export function sectionOfAbs(ix: SecIndex, a: number): number {
  return Math.max(0, lastAtOrBelow(ix.absStart, a));
}
export function effToAbs(ix: SecIndex, e: number): number {
  const s = sectionOfEff(ix, e);
  return (ix.absStart[s] ?? 0) + (e - (ix.effStart[s] ?? 0));
}
/** Effective index of absolute `a`, or -1 when its month is folded. */
export function absToEff(ix: SecIndex, a: number): number {
  const s = sectionOfAbs(ix, a);
  if ((ix.effCount[s] ?? 0) === 0) return -1;
  return (ix.effStart[s] ?? 0) + (a - (ix.absStart[s] ?? 0));
}
/** Effective range [lo, hi] (inclusive) as absolute ranges: folded months in between are not part of it. */
export function effRangeToAbs(ix: SecIndex, lo: number, hi: number): Range[] {
  if (ix.effTotal === 0) return [];
  lo = Math.max(0, lo); hi = Math.min(ix.effTotal - 1, hi);
  if (lo > hi) return [];
  const out: Range[] = [];
  for (let s = sectionOfEff(ix, lo); s < ix.count.length && (ix.effStart[s] ?? 0) <= hi; s++) {
    const c = ix.effCount[s] ?? 0;
    if (c === 0) continue;
    const st = ix.effStart[s]!;
    const a = Math.max(lo, st), b = Math.min(hi, st + c - 1);
    const base = ix.absStart[s]! - st;
    const prev = out[out.length - 1];
    if (prev && prev[1] + 1 === a + base) prev[1] = b + base;
    else out.push([a + base, b + base]);
  }
  return out;
}

// ---- selection as index ranges ---------------------------------------------------------------------------------

/** Inclusive absolute index range. A selection is a sorted list of disjoint, non-touching ranges. */
export type Range = [number, number];

export function rangeCount(rs: readonly Range[]): number {
  let n = 0;
  for (const [a, b] of rs) n += b - a + 1;
  return n;
}
export function rangeHas(rs: readonly Range[], i: number): boolean {
  let lo = 0, hi = rs.length - 1;
  while (lo <= hi) {
    const m = (lo + hi) >> 1;
    const [a, b] = rs[m]!;
    if (i < a) hi = m - 1; else if (i > b) lo = m + 1; else return true;
  }
  return false;
}
export function rangeAdd(rs: readonly Range[], add: readonly Range[]): Range[] {
  const all = [...rs, ...add].map((r): Range => [r[0], r[1]]).sort((x, y) => x[0] - y[0]);
  const out: Range[] = [];
  for (const r of all) {
    const last = out[out.length - 1];
    if (last && r[0] <= last[1] + 1) last[1] = Math.max(last[1], r[1]);
    else out.push(r);
  }
  return out;
}
export function rangeRemoveIndex(rs: readonly Range[], i: number): Range[] {
  const out: Range[] = [];
  for (const [a, b] of rs) {
    if (i < a || i > b) { out.push([a, b]); continue; }
    if (a < i) out.push([a, i - 1]);
    if (i < b) out.push([i + 1, b]);
  }
  return out;
}
/** Ctrl+click: the index joins the selection, or leaves it when it was in. */
export function rangeToggle(rs: readonly Range[], i: number): Range[] {
  return rangeHas(rs, i) ? rangeRemoveIndex(rs, i) : rangeAdd(rs, [[i, i]]);
}

// ---- the mosaic: one template for every month ------------------------------------------------------------------
// Tile j of a month sits where `tileSpan(j)` and the first-free-slot packing put it, whatever the month's size,
// so every section is the first `count` tiles of ONE packing (no tile object per photo, nothing per section).

export interface Template {
  cols: number;
  n: number;
  col: Uint8Array;
  row: Uint32Array;
  w: Uint8Array;
  h: Uint8Array;
  /** Rows a month of k photos spans (k = 0..n). */
  rowsAt: Uint32Array;
  /** First row that was still open when tile j was placed: non-decreasing, so a viewport band can be found by bisection. */
  rowMin: Uint32Array;
  /** How far below `rowMin` (plus the tile's height) a tile can land: the band search widens by this. */
  slack: number;
}

function span(j: number, cols: number): [number, number] {
  if (cols < 4) return [1, 1];
  if (j === 0 || j % 11 === 6) return [2, 2];
  if (j % 5 === 4) return [2, 1];
  return [1, 1];
}

/** The packing of the first `n` tiles for `cols` columns (a feature tile takes the first free slot, like grid dense flow). */
export function packTemplate(n: number, cols: number): Template {
  const col = new Uint8Array(n), row = new Uint32Array(n), w = new Uint8Array(n), h = new Uint8Array(n);
  const rowsAt = new Uint32Array(n + 1), rowMin = new Uint32Array(n);
  let slack = 1;
  const occ: Uint8Array[] = [];
  const rowOf = (r: number) => (occ[r] ??= new Uint8Array(cols));
  let minRow = 0, rows = 0;
  for (let j = 0; j < n; j++) {
    const [w0, hh] = span(j, cols);
    const ww = Math.min(w0, cols);
    let placed = false;
    for (let r = minRow; !placed; r++) {
      for (let c = 0; c + ww <= cols && !placed; c++) {
        let ok = true;
        for (let dr = 0; dr < hh && ok; dr++) for (let dc = 0; dc < ww; dc++) if (rowOf(r + dr)[c + dc]) { ok = false; break; }
        if (!ok) continue;
        for (let dr = 0; dr < hh; dr++) for (let dc = 0; dc < ww; dc++) rowOf(r + dr)[c + dc] = 1;
        col[j] = c; row[j] = r; w[j] = ww; h[j] = hh; rowMin[j] = minRow;
        slack = Math.max(slack, r - minRow + hh);
        rows = Math.max(rows, r + hh);
        placed = true;
      }
    }
    rowsAt[j + 1] = rows;
    while (occ[minRow]?.every(Boolean)) minRow++;
  }
  return { cols, n, col, row, w, h, rowsAt, rowMin, slack };
}

export interface Box { y: number; h: number; col: number; w: number; hs: number }

/** Month-relative box of tile j. `head` is the month's heading height. */
export function templateBox(t: Template, j: number, rowH: number, gap: number, head: number): Box {
  const hs = t.h[j] ?? 1;
  return { y: head + (t.row[j] ?? 0) * (rowH + gap), h: hs * rowH + (hs - 1) * gap, col: t.col[j] ?? 0, w: t.w[j] ?? 1, hs };
}

/** Tile range [from, to) of a month that can intersect the month-relative vertical band [y0, y1] (the caller filters exactly). */
export function tilesInBand(t: Template, count: number, rowH: number, gap: number, head: number, y0: number, y1: number): [number, number] {
  const r0 = Math.floor((y0 - head) / (rowH + gap)) - t.slack;
  const r1 = Math.floor((y1 - head) / (rowH + gap));
  let lo = 0, hi = count;
  while (lo < hi) { const m = (lo + hi) >> 1; if ((t.rowMin[m] ?? 0) < r0) lo = m + 1; else hi = m; }
  const from = lo;
  hi = count;
  while (lo < hi) { const m = (lo + hi) >> 1; if ((t.rowMin[m] ?? 0) <= r1) lo = m + 1; else hi = m; }
  return [from, lo];
}

/** Arrow-key move: left/right follow the order, up/down go to the nearest tile above / below (looked up within 64 either way). */
export function moveBy(i: number, key: string, n: number, boxAt: (k: number) => Box | null): number {
  if (n <= 0) return 0;
  switch (key) {
    case "ArrowLeft": return Math.max(0, i - 1);
    case "ArrowRight": return Math.min(n - 1, i + 1);
    case "Home": return 0;
    case "End": return n - 1;
  }
  if (key !== "ArrowUp" && key !== "ArrowDown") return i;
  const c = boxAt(i);
  if (!c) return i;
  const cx = c.col + c.w / 2;
  let best = -1, bestDy = Infinity, bestDx = Infinity;
  for (let k = Math.max(0, i - 64); k <= Math.min(n - 1, i + 64); k++) {
    if (k === i) continue;
    const b = boxAt(k);
    if (!b) continue;
    const dy = key === "ArrowDown" ? b.y - (c.y + c.h) : c.y - (b.y + b.h);
    if (dy < -1) continue;
    const dx = Math.abs(b.col + b.w / 2 - cx);
    if (dy < bestDy - 1 || (Math.abs(dy - bestDy) <= 1 && dx < bestDx)) { best = k; bestDy = dy; bestDx = dx; }
  }
  return best >= 0 ? best : i;
}

// ---- row pages -------------------------------------------------------------------------------------------------

export const PAGE = 256;

/** Fetched rows by absolute index, in pages, with the nearest-to-the-viewport eviction the result list uses. */
export class RowCache<T> {
  private pages = new Map<number, T[]>();
  readonly page: number;
  readonly cap: number;
  constructor(page = PAGE, cap = 48) { this.page = page; this.cap = cap; }
  get(i: number): T | undefined {
    return this.pages.get(Math.floor(i / this.page))?.[i % this.page];
  }
  has(p: number): boolean { return this.pages.has(p); }
  get size(): number { return this.pages.size; }
  put(p: number, rows: T[]): void { this.pages.set(p, rows); }
  clear(): void { this.pages.clear(); }
  /** Pages covering absolute rows [start, end) that are not loaded. */
  missing(start: number, end: number): number[] {
    const out: number[] = [];
    for (let p = Math.floor(start / this.page); p * this.page < end; p++) if (!this.pages.has(p)) out.push(p);
    return out;
  }
  /** While over the cap, drop the pages farthest from the wanted spans (flat [start, end) pairs, the first one being the
   *  viewport); pages inside any span and page 0 stay. Returns the dropped pages. */
  trim(spans: readonly number[]): number[] {
    if (this.pages.size <= this.cap || spans.length < 2) return [];
    const keep = new Set<number>([0]);
    for (let i = 0; i + 1 < spans.length; i += 2) {
      for (let p = Math.floor(spans[i]! / this.page); p * this.page < spans[i + 1]!; p++) keep.add(p);
    }
    const mid = (spans[0]! + spans[1]!) / 2 / this.page;
    const far = [...this.pages.keys()].filter((p) => !keep.has(p)).sort((a, b) => Math.abs(b - mid) - Math.abs(a - mid));
    const drop = far.slice(0, this.pages.size - this.cap);
    for (const p of drop) this.pages.delete(p);
    return drop;
  }
}
