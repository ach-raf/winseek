import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit } from "./types";
import { CHUNK, MAX_INFLIGHT, chunksToEvict, wantedChunks } from "./sparseGrid";

/** One name stream addressed by position, read by chunk near the viewport (see sparseGrid). */
export interface PicChunks {
  /** Positions in the stream (the excluded paths already taken out); null until the first answer. */
  total: number | null;
  /** The hit at position `j`, once its chunk is in. */
  at: (j: number) => Hit | undefined;
  /** The grid draws positions `[from, to)` of this stream now: read what is near them. */
  view: (from: number, to: number) => void;
  /** Files changed: read the held chunks again (they stay on screen until the new answer replaces them). */
  refresh: () => void;
  /** A chunk the view wants is still on its way. */
  busy: boolean;
}

interface Snap { key: string; total: number | null; chunks: ReadonlyMap<number, Hit[]>; busy: boolean }

/** What the requests are bookkept by (touched only in effects and callbacks, never while rendering). */
interface Book {
  key: string;
  query: string;
  exclude: string[];
  /** The visible positions, as the grid last reported them. */
  from: number;
  to: number;
  want: Set<number>;
  inflight: Set<number>;
  /** Held chunks to read again (after a file change). */
  stale: Set<number>;
  total: number | null;
  chunks: Map<number, Hit[]>;
}

const fresh = (key: string, query = "", exclude: string[] = []): Book => ({
  key, query, exclude, from: 0, to: 0, want: new Set(), inflight: new Set(), stale: new Set(), total: null, chunks: new Map(),
});

type Search = Pick<Backend, "search">["search"];

/** Ask for the chunks near the view that are neither held nor on their way, at most MAX_INFLIGHT at once. */
function pump(b: Book, current: () => Book, search: Search, publish: (b: Book) => void): void {
  if (!b.key || current() !== b) return;
  // Before the first answer the length is unknown: only the first chunk is asked for.
  const want = b.total === null ? [0] : wantedChunks(b.from, b.to, b.total);
  b.want = new Set(want);
  for (const c of want) {
    if (b.inflight.size >= MAX_INFLIGHT) break;
    if ((b.chunks.has(c) && !b.stale.has(c)) || b.inflight.has(c)) continue;
    b.inflight.add(c);
    search({ query: b.query, offset: c * CHUNK, limit: CHUNK, sort: "name", filter: "images", stream: "picgrid", exclude: b.exclude })
      .then((r) => {
        if (current() !== b) return; // another list since: the answer belongs to nobody
        b.inflight.delete(c);
        b.stale.delete(c);
        b.total = r.total;
        // Still near the view: keep it (letting the farthest go past the cap); else it is not held at all.
        if (b.want.has(c)) {
          b.chunks.set(c, r.hits);
          for (const d of chunksToEvict(b.chunks.keys(), (b.from + b.to) / 2, b.want)) b.chunks.delete(d);
        }
        publish(b);
        pump(b, current, search, publish);
      }, (e: unknown) => {
        if (current() !== b) return;
        b.inflight.delete(c);
        console.error(e);
      });
  }
}

/**
 * `query`'s picture names (name order) without the `exclude` paths, by chunk: the first chunk is asked for at
 * once (its answer carries the total, so the grid has its full height from the start), the others when the view
 * comes within a viewport of them. An answer for a chunk the view has left meanwhile is dropped, and past
 * MAX_CHUNKS the farthest chunks are let go. `exclude` must keep its identity while its content does.
 */
export function usePicChunks(backend: Pick<Backend, "search">, o: { query: string; exclude: readonly string[]; on: boolean; epoch: number }): PicChunks {
  const { query, exclude, on, epoch } = o;
  const key = `${query}\u0001${epoch}\u0001${exclude.join("\u0001")}`;
  const book = useRef<Book>(fresh(""));
  const [snap, setSnap] = useState<Snap>({ key: "", total: null, chunks: new Map(), busy: false });

  const publish = useCallback((b: Book) => {
    setSnap({ key: b.key, total: b.total, chunks: new Map(b.chunks), busy: Array.from(b.want).some((c) => !b.chunks.has(c)) });
  }, []);
  const run = useCallback(() => pump(book.current, () => book.current, (a) => backend.search(a), publish), [backend, publish]);

  useEffect(() => {
    book.current = on ? fresh(key, query, [...exclude]) : fresh("");
    if (!on) return;
    publish(book.current);
    run();
  }, [on, key, query, exclude, run, publish]);

  const view = useCallback((from: number, to: number) => {
    const b = book.current;
    if (b.from === from && b.to === to) return;
    b.from = from;
    b.to = to;
    run();
  }, [run]);

  const refresh = useCallback(() => {
    const b = book.current;
    for (const c of b.chunks.keys()) b.stale.add(c);
    run();
  }, [run]);

  const live = on && snap.key === key;
  const chunks = snap.chunks;
  const at = useMemo(() => (j: number): Hit | undefined => (live ? chunks.get(Math.floor(j / CHUNK))?.[j % CHUNK] : undefined), [live, chunks]);
  return { total: live ? snap.total : null, at, view, refresh, busy: live && (snap.total === null || snap.busy) };
}
