/**
 * The one picture grid every surface draws (search Pictures, Timeline, Collections): uniform tiles of one exact
 * size, so the layout is arithmetic (row = floor(i / cols)) and never depends on what a picture contains.
 * The numbers mirror `picgrid.css` (`--pic-w`, `--pic-h`, `--pic-gap`); keep the two in step.
 */

/** Tile width / height in css px (4:3) and the gap between tiles. */
export const PIC_W = 176;
export const PIC_H = 132;
export const PIC_GAP = 8;
/** Fewest columns any grid shows: below this the tiles shrink (same aspect) instead of dropping a column. */
export const MIN_COLS = 3;

export interface GridMetrics {
  cols: number;
  tileW: number;
  tileH: number;
  gap: number;
  /** Distance from one row's top to the next row's top. */
  rowStride: number;
  /** Width the tiles take (cols * tileW + gaps). */
  width: number;
}

/**
 * Columns for `innerWidth` css px: as many exact-size tiles as fit (at least MIN_COLS, at most `maxCols`).
 * Tile size never changes with the width unless even MIN_COLS do not fit, so every surface shows the same tile.
 */
export function gridMetrics(innerWidth: number, maxCols = Infinity, fill = false): GridMetrics {
  const w = Math.max(0, innerWidth);
  const fit = Math.floor((w + PIC_GAP) / (PIC_W + PIC_GAP));
  const cols = Math.max(MIN_COLS, Math.min(maxCols, fit));
  let tileW = PIC_W;
  if (cols * PIC_W + (cols - 1) * PIC_GAP > w && w > 0) tileW = Math.max(24, Math.floor((w - (cols - 1) * PIC_GAP) / cols));
  // `fill`: spread the leftover width over the tiles so the grid's right edge meets the right gutter exactly
  // (the left edge already sits on the left gutter) instead of leaving a dead strip beside the rail.
  else if (fill && w > 0) tileW = Math.floor((w - (cols - 1) * PIC_GAP) / cols);
  const tileH = Math.round((tileW * 3) / 4);
  return { cols, tileW, tileH, gap: PIC_GAP, rowStride: tileH + PIC_GAP, width: cols * tileW + (cols - 1) * PIC_GAP };
}

export const rowOf = (i: number, cols: number): number => Math.floor(i / Math.max(1, cols));

/** Rows a grid of `count` tiles spans. */
export const rowsFor = (count: number, cols: number): number => (count <= 0 ? 0 : Math.ceil(count / Math.max(1, cols)));

/**
 * Tile index range [from, to) of a grid of `count` tiles whose first row starts at `top` (document px) that can be
 * seen from a viewport at `scrollTop` with height `vh`, plus `overscanRows` rows either way.
 */
export function rangeForViewport(scrollTop: number, vh: number, g: Pick<GridMetrics, "cols" | "rowStride">, count: number, top = 0, overscanRows = 2): [number, number] {
  if (count <= 0 || g.rowStride <= 0) return [0, 0];
  const rows = rowsFor(count, g.cols);
  const r0 = Math.max(0, Math.floor((scrollTop - top) / g.rowStride) - overscanRows);
  const r1 = Math.min(rows, Math.ceil((scrollTop + vh - top) / g.rowStride) + overscanRows);
  if (r1 <= r0) return [0, 0];
  return [r0 * g.cols, Math.min(count, r1 * g.cols)];
}

export interface SectionLayout { top: number[]; h: number[]; total: number }

/**
 * Stacked sections (months) of one grid: each is its heading (`heads[s]` px) then its rows. A section with no
 * tiles (folded) is just its heading plus `emptyH`. Everything is arithmetic from the counts.
 */
export function sectionLayout(counts: readonly number[], heads: readonly number[], g: Pick<GridMetrics, "cols" | "rowStride" | "gap">, bottom: number, emptyH = 6): SectionLayout {
  const top: number[] = [], h: number[] = [];
  let y = 0;
  counts.forEach((n, s) => {
    top.push(y);
    const hh = (heads[s] ?? 0) + (n > 0 ? rowsFor(n, g.cols) * g.rowStride - g.gap + bottom : emptyH);
    h.push(hh);
    y += hh;
  });
  return { top, h, total: y };
}

export type GridDir = "up" | "down" | "left" | "right";

/**
 * One step inside a grid of `count` tiles in `cols` columns from position `pos`. Left / right stay in the row; up /
 * down move a whole row; from the last full row down goes to the last tile of a shorter row below. Leaving the grid
 * answers -1 (above) or `count` (below), so the owner decides where the next section starts.
 */
export function gridStep(pos: number, count: number, cols: number, dir: GridDir): number {
  const n = Math.max(1, cols);
  const last = count - 1;
  switch (dir) {
    case "left": return pos % n === 0 ? pos : pos - 1;
    case "right": return pos === last || pos % n === n - 1 ? pos : pos + 1;
    case "down": {
      if (pos + n <= last) return pos + n;
      const lastRowStart = Math.floor(last / n) * n;
      return pos < lastRowStart ? last : count;
    }
    case "up": return pos >= n ? pos - n : -1;
  }
}

/** The arrow / paging key a grid answers, or null. */
export function gridKey(key: string): GridDir | "home" | "end" | "pageup" | "pagedown" | null {
  switch (key) {
    case "ArrowUp": return "up";
    case "ArrowDown": return "down";
    case "ArrowLeft": return "left";
    case "ArrowRight": return "right";
    case "Home": return "home";
    case "End": return "end";
    case "PageUp": return "pageup";
    case "PageDown": return "pagedown";
    default: return null;
  }
}

/**
 * Keyboard movement through a grid made of sections (months), each `counts[s]` tiles in the same `cols` columns.
 * `i` is the flat index over all sections. Up / down past a section's edge continue in the neighbouring section in
 * the same column (clamped to its tiles); PageUp / PageDown move `pageRows` rows the same way; Home / End jump to
 * the first / last tile.
 */
export function gridMove(i: number, key: string, counts: readonly number[], cols: number, pageRows = 1): number {
  const total = counts.reduce((a, n) => a + n, 0);
  if (total <= 0) return 0;
  const k = gridKey(key);
  const cur = Math.max(0, Math.min(total - 1, i));
  if (k === null) return cur;
  if (k === "home") return 0;
  if (k === "end") return total - 1;
  const n = Math.max(1, cols);
  if (k === "pageup" || k === "pagedown") {
    let at = cur;
    for (let r = 0; r < Math.max(1, pageRows); r++) {
      const next = gridMove(at, k === "pagedown" ? "ArrowDown" : "ArrowUp", counts, n);
      if (next === at) break;
      at = next;
    }
    return at;
  }
  // section of `cur`
  let s = 0, start = 0;
  while (s < counts.length && cur >= start + (counts[s] ?? 0)) { start += counts[s] ?? 0; s++; }
  const count = counts[s] ?? 0;
  const pos = cur - start;
  const step = gridStep(pos, count, n, k);
  if (step >= 0 && step < count) return start + step;
  const col = pos % n;
  if (step < 0) {
    // the previous non-empty section's last row, same column
    let ps = s - 1, pStart = start;
    while (ps >= 0 && (counts[ps] ?? 0) === 0) ps--;
    if (ps < 0) return cur;
    for (let x = s - 1; x >= ps; x--) pStart -= counts[x] ?? 0;
    const pc = counts[ps]!;
    const lastRow = Math.floor((pc - 1) / n);
    return pStart + Math.min(pc - 1, lastRow * n + col);
  }
  let ns = s + 1, nStart = start + count;
  while (ns < counts.length && (counts[ns] ?? 0) === 0) ns++;
  if (ns >= counts.length) return cur;
  for (let x = s + 1; x < ns; x++) nStart += counts[x] ?? 0;
  return nStart + Math.min((counts[ns] ?? 1) - 1, col);
}

/** Quick Look keys of a gallery: Space toggles, Escape closes (only while open, so it still reaches the window otherwise). */
export function quickLookKey(key: string, open: boolean): "toggle" | "close" | null {
  if (key === " " || key === "Spacebar") return "toggle";
  if (key === "Escape" && open) return "close";
  return null;
}
