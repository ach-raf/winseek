/**
 * Virtual-list geometry for the results table.
 *
 * Rows are absolutely positioned inside a spacer div whose height stands in for the whole
 * list, so the scrollbar reaches every row. One hard limit: Chromium clamps layout objects
 * and scroll ranges at ~33.5M px, which a 600k-row list (54 px rows) would blow past — rows
 * would clamp to one spot and the tail of the list would be unreachable. Past the cap the
 * spacer stops growing and the scroll position is mapped onto the real list proportionally
 * (a "proxy scrollbar", like Everything's): the visible window is always right, only the
 * scrollbar granularity compresses.
 */

/** Stays under Chromium's ~33,554,428 px layout cap. */
export const VLIST_MAX_PX = 30_000_000;

export interface VListGeom {
  /** Height for the spacer div (px). */
  height: number;
  /** Virtual offset the visible window starts at (px into the whole list). */
  voff: number;
  /**
   * Where that window sits inside the spacer (the clamped scroll position). Rows are positioned in spacer
   * coordinates, so row `i` goes at `top + i * rowH - voff`: under the viewport wherever the scrollbar is,
   * and never past the spacer's capped height.
   */
  top: number;
  /** First visible row index (with overscan, clamped at 0). */
  first: number;
  /** Last visible row index (with overscan, clamped at total-1). */
  last: number;
}

/**
 * `pin` is an exact virtual offset that overrides the scrollbar mapping. Past the cap one
 * scrollbar pixel stands for many list pixels, so wheel and keyboard moves are tracked
 * exactly here (and the scrollbar thumb is only moved to roughly match, see `scrollTopFor`).
 */
export function vlist(total: number, rowH: number, scrollTop: number, viewH: number, overscan: number, pin?: number | null): VListGeom {
  const px = total * rowH;
  const clipped = px > VLIST_MAX_PX;
  const spacer = clipped ? VLIST_MAX_PX : px;
  const maxVoff = Math.max(0, px - viewH);
  // Map the scroll position onto the (possibly clipped) list: the top of the scrollbar
  // shows row 0, the bottom shows the last row. Both ranges exclude one viewport.
  const from = Math.max(0, Math.min(scrollTop, spacer - viewH));
  const mapped = clipped ? (from / Math.max(1, spacer - viewH)) * maxVoff : from;
  const voff = clipped && pin != null ? Math.max(0, Math.min(pin, maxVoff)) : mapped;
  return {
    height: spacer,
    voff,
    top: from,
    first: Math.max(0, Math.floor(voff / rowH) - overscan),
    last: Math.min(total - 1, Math.ceil((voff + viewH) / rowH) + overscan),
  };
}

/** True when the list is past the cap (the scrollbar is a proxy and moves need `pin`). */
export function vlistClipped(total: number, rowH: number): boolean {
  return total * rowH > VLIST_MAX_PX;
}

/** The scrollbar position that shows virtual offset `voff` (the inverse of the mapping above). */
export function scrollTopFor(total: number, rowH: number, viewH: number, voff: number): number {
  const px = total * rowH;
  if (px <= VLIST_MAX_PX) return voff;
  return (voff / Math.max(1, px - viewH)) * (VLIST_MAX_PX - viewH);
}

/**
 * Which result pages to keep when the cache is over `cap`: the ones nearest the visible
 * window. Returns the pages to drop (farthest first).
 */
export function pagesToEvict(loaded: Iterable<number>, firstPage: number, lastPage: number, cap: number): number[] {
  const mid = (firstPage + lastPage) / 2;
  const all = [...loaded];
  if (all.length <= cap) return [];
  return all
    .filter((p) => p !== 0 && (p < firstPage || p > lastPage)) // page 0 feeds the overview; visible pages stay
    .sort((a, b) => Math.abs(b - mid) - Math.abs(a - mid))
    .slice(0, all.length - cap);
}
