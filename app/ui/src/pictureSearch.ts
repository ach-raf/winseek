import type { Backend, Hit } from "./types";

/**
 * The picture search every surface runs (search popup Pictures, Library Timeline filter, Collections filter): the
 * name index's picture streams (`pics` = the typed words as words, `tail` = the letters inside another word) read
 * in pages, fused with the meaning hits by `rankPictures`. This file is the reading part; `usePictureSearch` wires
 * it to React, App's popup reads through the same function.
 */

/** One request's worth of names. */
export const PIC_SEARCH_PAGE = 400;
/** Most picture names ever held in the page: a one-letter search can match a whole library, and every held hit is
 *  JS memory and ranking work. Scrolling past this many shows no more (the count still says how many there are). */
export const PIC_SEARCH_CAP = 20_000;
/** While pages keep landing, the list is handed on at most this often (one ranking per burst, not per page). */
export const EMIT_MS = 120;

export interface NamesState {
  total: number;
  /** Nothing more will be read for this limit (the end, the cap, or the limit was reached). */
  done: boolean;
  /** The stream itself is exhausted (or capped): raising the limit reads nothing more. */
  complete: boolean;
}

/**
 * Read a name stream into `acc` (which survives across calls with the same query: a raised limit appends, nothing
 * is re-read) until `limit`, the end, or the cap. `emit` fires after the first page, then at most every `EMIT_MS`
 * while pages keep coming, and once at the end. `live()` false = superseded: stop without emitting.
 */
export async function readPictureNames(
  backend: Pick<Backend, "search">,
  o: {
    query: string;
    stream: "pics" | "tail";
    /** The hits read so far and the stream's total (-1 = not asked yet). */
    acc: { hits: Hit[]; total: number };
    limit: number;
    page?: number;
    live: () => boolean;
    emit: (s: NamesState) => void;
    now?: () => number;
  },
): Promise<void> {
  const page = o.page ?? PIC_SEARCH_PAGE;
  const limit = Math.min(o.limit, PIC_SEARCH_CAP);
  const now = o.now ?? (() => performance.now());
  let last = -Infinity;
  let first = true;
  for (;;) {
    const want = Math.min(page, limit - o.acc.hits.length);
    if (want <= 0) { o.emit({ total: Math.max(o.acc.total, o.acc.hits.length), done: true, complete: o.acc.hits.length >= PIC_SEARCH_CAP }); return; }
    const r = await backend.search({ query: o.query, offset: o.acc.hits.length, limit: want, sort: "name", filter: "images", stream: o.stream });
    if (!o.live()) return;
    for (const h of r.hits) o.acc.hits.push(h);
    o.acc.total = r.total;
    const complete = r.hits.length === 0 || o.acc.hits.length >= r.total || o.acc.hits.length >= PIC_SEARCH_CAP;
    const done = complete || o.acc.hits.length >= limit;
    const t = now();
    if (first || done || t - last >= EMIT_MS) {
      o.emit({ total: r.total, done, complete });
      last = t;
      first = false;
    }
    if (done) return;
  }
}
