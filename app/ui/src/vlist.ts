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
  /** Virtual offset the visible window starts at (px): rows render at `i * rowH - voff`. */
  voff: number;
  /** First visible row index (with overscan, clamped at 0). */
  first: number;
  /** Last visible row index (with overscan, clamped at total-1). */
  last: number;
}

export function vlist(total: number, rowH: number, scrollTop: number, viewH: number, overscan: number): VListGeom {
  const px = total * rowH;
  const clipped = px > VLIST_MAX_PX;
  const spacer = clipped ? VLIST_MAX_PX : px;
  // Map the scroll position onto the (possibly clipped) list: the top of the scrollbar
  // shows row 0, the bottom shows the last row. Both ranges exclude one viewport.
  const from = Math.max(0, Math.min(scrollTop, spacer - viewH));
  const voff = clipped ? (from / Math.max(1, spacer - viewH)) * Math.max(0, px - viewH) : from;
  return {
    height: spacer,
    voff,
    first: Math.max(0, Math.floor(voff / rowH) - overscan),
    last: Math.min(total - 1, Math.ceil((voff + viewH) / rowH) + overscan),
  };
}
