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
