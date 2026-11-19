import { useEffect, useMemo, useRef, useState } from "react";
import type { Backend, TlRow, TlSummary } from "./types";
import type { GallerySource, GallerySec } from "./Gallery";
import { PAGE, RowCache } from "./timelineLogic";

/** Pages of rows kept (about 12k photos); the ones nearest the viewport win (see `pagesToEvict`). */
const CACHE_PAGES = 48;

/** The same list when its months and counts are the same: a re-read of an unchanged library keeps the selection and the rows on screen. */
export function listSig(s: TlSummary): string {
  return `${s.total}|${s.sections.map((x) => `${x.key}:${x.count}`).join(",")}`;
}

/**
 * The timeline as a `GallerySource`: month sections known up front, rows fetched by absolute index in pages of
 * 256 as the viewport reaches them and kept in a small cache. A list with a new `rev` and the same shape keeps its
 * cached rows on screen (they are re-fetched in place); a different shape starts empty.
 */
export function useTimelineSource(backend: Backend, summary: TlSummary | null): GallerySource | null {
  const cache = useRef(new RowCache<TlRow>(PAGE, CACHE_PAGES));
  const fresh = useRef(new Set<number>()); // pages loaded for the current rev
  const pending = useRef(new Set<number>());
  const cur = useRef<{ rev: number; sig: string } | null>(null);
  const [ver, setVer] = useState(0);
  const bump = useRef(0);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const sig = summary ? listSig(summary) : "";
  // Idempotent bookkeeping for a new list; it must run before the rows are read for it (hence not an effect).
  const rev = summary?.rev ?? -1;
  const prev = cur.current;
  if (summary && (!prev || prev.rev !== rev)) {
    if (!prev || prev.sig !== sig) cache.current.clear();
    fresh.current.clear();
    pending.current.clear();
    cur.current = { rev, sig };
  }

  const secs = useMemo<GallerySec[]>(
    () => (summary?.sections ?? []).map((s) => ({ key: s.key, label: s.label, year: s.year, month: s.month, count: s.count, bytes: s.bytes })),
    [summary],
  );

  return useMemo<GallerySource | null>(() => {
    if (!summary) return null;
    const rv = summary.rev;
    const arrived = () => {
      // one render for a burst of arriving pages
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => { bump.current++; setVer(bump.current); }, 0);
    };
    return {
      id: sig,
      secs,
      total: summary.total,
      ver,
      get: (abs) => cache.current.get(abs),
      want: (spans) => {
        const c = cache.current;
        for (let i = 0; i + 1 < spans.length; i += 2) {
          const start = spans[i]!, end = Math.min(spans[i + 1]!, summary.total);
          for (let p = Math.floor(start / PAGE); p * PAGE < end; p++) {
            if (fresh.current.has(p) || pending.current.has(p)) continue;
            pending.current.add(p);
            backend.libraryTimelineRows(rv, p * PAGE, PAGE).then((rows) => {
              if (cur.current?.rev !== rv) return; // a newer list took over; its own wants follow
              pending.current.delete(p);
              if (!rows) return;
              c.put(p, rows);
              fresh.current.add(p);
              arrived();
            }).catch(() => { pending.current.delete(p); });
          }
        }
        for (const p of c.trim(spans)) fresh.current.delete(p); // over the cap: the pages farthest from the viewport go
      },
      pick: (ranges, withPaths) => backend.libraryTimelinePick(rv, ranges, withPaths),
    };
    // `ver` is in the deps on purpose: arriving rows give the grid a new source object to render from.
  }, [backend, summary, sig, secs, ver]);
}
