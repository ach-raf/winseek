import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit } from "./types";
import { useSemantic } from "./hooks";
import { hasRanges, nameQuery, parseQuery, rangeClause } from "./filters";
import { rankPictures, wordQuery, type Pic } from "./pictureRank";
import { semAccepts } from "./Semantic";
import { PIC_SEARCH_CAP, PIC_SEARCH_PAGE, readPictureNames } from "./pictureSearch";

/** Names read before the user scrolls; every `loadMore` reads this many more. */
const FIRST = PIC_SEARCH_PAGE * 3;
const MORE = PIC_SEARCH_PAGE * 5;

export interface PictureSearch {
  /** The trimmed query these results answer ("" = no search). */
  query: string;
  /** Best first: what the picture shows + names that have the words as words, then (last) names that only hold
   *  the letters inside another word and were not found by meaning — the popup's "Other names containing". */
  items: Pic[];
  /** How many pictures the search has in all (at least `items.length`; more while pages are still unread). */
  total: number;
  loading: boolean;
  /** The view reached the end of `items`: read the next pages (up to PIC_SEARCH_CAP). */
  loadMore(): void;
}

interface Acc { key: string; hits: Hit[]; total: number }
interface Names { key: string; hits: Hit[]; total: number; done: boolean; complete: boolean }

/** One name stream for `query`, read in pages up to `limit` into an accumulator that survives a raised limit. */
function useNameStream(backend: Backend, query: string, stream: "pics" | "tail", on: boolean, limit: number, epoch: number): Names | null {
  const key = `${stream}|${query}|${epoch}`;
  const acc = useRef<Acc>({ key: "", hits: [], total: -1 });
  const [st, setSt] = useState<Names | null>(null);
  useEffect(() => {
    if (!on) return;
    let dead = false;
    if (acc.current.key !== key) acc.current = { key, hits: [], total: -1 };
    const a = acc.current;
    readPictureNames(backend, {
      query, stream, acc: a, limit,
      live: () => !dead,
      emit: (s) => setSt({ key, hits: a.hits.slice(), total: s.total, done: s.done, complete: s.complete }),
    }).catch((e) => { if (!dead) setSt((p) => (p && p.key === key ? { ...p, done: true, complete: true } : p)); console.error(e); });
    return () => { dead = true; };
  }, [backend, key, query, stream, on, limit]);
  return on && st?.key === key ? st : null;
}

/**
 * The search popup's picture search as a hook, for every other surface that filters pictures (Library Timeline,
 * Collections): the same query parsing (date / size chips ride along as index terms), the same name-index streams
 * (`pics`: words as words, `tail`: letters inside another word), the same meaning search (every relevant picture,
 * paged) and the same fuse (`rankPictures`). Memory is bounded: names are read in pages of 400 as the view asks
 * (`loadMore`), never past PIC_SEARCH_CAP, and the ranking runs once per burst of pages, not per page.
 */
export function usePictureSearch(backend: Backend, query: string, opts: { enabled: boolean; semantic: boolean; epoch?: number }): PictureSearch {
  const q = query.trim();
  const epoch = opts.epoch ?? 0;
  const parsed = useMemo(() => parseQuery(q), [q]);
  const nameQ = nameQuery(parsed);
  const range = rangeClause(parsed);
  const picQ = `${wordQuery(nameQ)} ${range}`.trim();
  const tailQ = `${nameQ} ${range}`.trim();
  const on = opts.enabled && q !== "" && picQ !== "";

  // How many names the view may hold so far; a new query starts at FIRST again.
  const [lim, setLim] = useState({ q: picQ, n: FIRST });
  const limit = lim.q === picQ ? lim.n : FIRST;
  if (lim.q !== picQ) setLim({ q: picQ, n: FIRST });

  const pics = useNameStream(backend, picQ, "pics", on, limit, epoch);
  // The letters-inside-a-word names come after every word match: read once the word stream is exhausted.
  const tailOn = on && tailQ !== picQ && !!pics?.complete && pics.hits.length < limit;
  const tail = useNameStream(backend, tailQ, "tail", tailOn, Math.max(PIC_SEARCH_PAGE, limit - (pics?.hits.length ?? 0)), epoch);

  const sem = useSemantic(backend, parsed.text, on && opts.semantic, "images", epoch, hasRanges(parsed) || !!parsed.ext, true);
  const semHits = useMemo(() => sem.hits.filter((h) => semAccepts(h, parsed, "images")), [sem.hits, parsed]);

  // One ranking per hand-off (readPictureNames emits once per burst of pages), never per page.
  const ranked = useMemo(() => {
    if (!on) return null;
    const names: { hit: Hit }[] = [];
    for (const h of pics?.hits ?? []) names.push({ hit: h });
    for (const h of tail?.hits ?? []) names.push({ hit: h });
    return rankPictures({ sem: semHits, names, text: parsed.text });
  }, [on, pics, tail, semHits, parsed.text]);
  const items = useMemo(() => (ranked ? [...ranked.ranked, ...ranked.tail] : []), [ranked]);

  const loading = on && (!pics || !pics.done || sem.loading || (tailOn && (!tail || !tail.done)));
  const more = on && !!pics && (!pics.complete || (tailOn && !!tail && !tail.complete)) && limit < PIC_SEARCH_CAP;
  const total = more ? Math.max(items.length, (pics?.total ?? 0) + (tail?.total ?? 0)) : items.length;
  const loadMore = useCallback(() => {
    if (!more || loading) return;
    setLim((l) => (l.q === picQ ? { q: picQ, n: Math.min(PIC_SEARCH_CAP, l.n + MORE) } : l));
  }, [more, loading, picQ]);
  return { query: on ? q : "", items, total, loading, loadMore };
}
