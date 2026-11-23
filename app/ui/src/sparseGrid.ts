// A grid of any length whose rows are read from the name index in fixed-size chunks, only near what is on
// screen (Immich's timeline: the size is known up front, the data comes by chunk where the viewport is).
// The scroll height is the whole list from the first answer; a jump to row 600,000 asks for the chunks
// there and nothing in between; far-away chunks are dropped again, so memory stays bounded whatever
// the list's length. This file is the bookkeeping (pure, tested); usePicChunks wires it to React.

/** Positions per request. */
export const CHUNK = 500;
/** Most chunks held at once (20,000 rows); the farthest from the viewport go first. */
export const MAX_CHUNKS = 40;
/** Requests on the wire at once: a fast drag queues no backlog of chunks it has already left behind. */
export const MAX_INFLIGHT = 2;

/**
 * The chunks to hold for the visible positions `[from, to)` of a list of `size`: every chunk within one
 * viewport height (`to - from` positions) above or below, nearest to the view first.
 */
export function wantedChunks(from: number, to: number, size: number, chunk = CHUNK): number[] {
  if (size <= 0 || to <= from) return [];
  const span = to - from;
  const lo = Math.max(0, from - span), hi = Math.min(size, to + span);
  if (hi <= lo) return [];
  const first = Math.floor(lo / chunk), last = Math.floor((hi - 1) / chunk);
  const mid = (from + to) / 2;
  const out: number[] = [];
  for (let c = first; c <= last; c++) out.push(c);
  return out.sort((a, b) => dist(a, mid, chunk) - dist(b, mid, chunk) || a - b);
}

/** Distance (positions) from chunk `c` to position `at`; 0 when `at` is inside it. */
function dist(c: number, at: number, chunk: number): number {
  const a = c * chunk, b = a + chunk;
  return at < a ? a - at : at >= b ? at - b + 1 : 0;
}

/** The held chunks to drop so at most `max` stay: the farthest from position `at` first. Wanted chunks stay. */
export function chunksToEvict(held: Iterable<number>, at: number, keep: ReadonlySet<number>, max = MAX_CHUNKS, chunk = CHUNK): number[] {
  const all = Array.from(held);
  if (all.length <= max) return [];
  const spare = all.filter((c) => !keep.has(c)).sort((a, b) => dist(b, at, chunk) - dist(a, at, chunk));
  return spare.slice(0, all.length - max);
}

/** Chromium lays out no box taller than 2^25 px (33,554,432): a million pictures in four columns are taller than
 *  that, so a taller list is drawn compressed into this height. */
export const MAX_PX = 30_000_000;

/**
 * A list `full` px tall drawn in at most `max` px: the list offset `y` that shows at the viewport top when it sits
 * `real` px into the drawn box, and the drawn height `h`. Within `edge` px of either end the two move 1:1 (rows
 * scroll at their own speed, the first and the last rows sit exactly at the ends); between, a scroll position maps
 * by its fraction, so dragging the scrollbar to 50% shows the middle of the list.
 */
export function compress(real: number, full: number, viewH: number, edge: number, max = MAX_PX): { y: number; h: number } {
  if (full <= max) return { y: real, h: full };
  const span = max - viewH, fullSpan = full - viewH;
  if (real <= edge) return { y: real, h: max };
  if (real >= span - edge) return { y: real + (fullSpan - span), h: max };
  return { y: edge + ((real - edge) * (fullSpan - 2 * edge)) / (span - 2 * edge), h: max };
}
