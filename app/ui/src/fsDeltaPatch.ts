import type { QueryClient, QueryKey } from "@tanstack/react-query";
import { qk, refreshLibraryIndex, refreshSpace } from "./qk.ts";
import { hitAt, makeMatcher, maybeImage, movedTo, patchDetail, patchUsage, pathKey, pruneGroups, pruneHits, pruneList, touchedAncestors } from "./fsDelta.ts";
import type { DiskUsage, DupGroup, FsDelta, Hit, LibImage, UsageDetail } from "./types.ts";

/**
 * Applies one file-system delta to a window's react-query cache: every list that can be patched is patched in place
 * (`setQueryData`, no re-read); only what cannot be patched (a new file's picture facts, a folder roll-up's totals) is
 * re-read, and only for the keys the delta touched that are cached right now. Pure over the QueryClient (node-testable).
 */

export interface DeltaPlan {
  /** Keys to re-read soon (coalesced ~0.3 s). */
  soon: QueryKey[];
  /** Keys to re-read lazily (coalesced ~10 s): lists a single new / grown file rarely changes (the biggest files). */
  later: QueryKey[];
}

export interface PatchOpts {
  /** The timeline's own hidden set already covers these removals (the Library's recycle): its base list is not re-read. */
  timelineCovered?: boolean;
}

const hasExt = (p: string) => { const n = p.slice(p.replace(/\//g, "\\").lastIndexOf("\\") + 1); return n.lastIndexOf(".") > 0; };
/** A picture by its extension (folders and extension-less files do not count). */
const isPic = (p: string) => hasExt(p) && maybeImage(p);

/** Every cached entry under `queryKey` through `fn`; written back only when it changed (an unchanged entry keeps its
 *  identity and its observers stay quiet). */
function patchAll<T>(qc: QueryClient, queryKey: QueryKey, fn: (old: T) => T): void {
  for (const [key, old] of qc.getQueriesData<T>({ queryKey })) {
    if (old === undefined) continue;
    const next = fn(old);
    if (next !== old) qc.setQueryData(key, next);
  }
}

/** Patch the cache for `d`; returns what still has to be re-read. An `overflow` delta re-reads the old way at once. */
export function patchCaches(qc: QueryClient, d: FsDelta, opts: PatchOpts = {}): DeltaPlan {
  const plan: DeltaPlan = { soon: [], later: [] };
  if (d.overflow) {
    refreshLibraryIndex(qc);
    refreshSpace(qc);
    void qc.invalidateQueries({ queryKey: qk.libStatus });
    return plan;
  }
  const m = makeMatcher(d);
  const cached = (key: QueryKey) => qc.getQueryData(key) !== undefined;

  if (!m.idle) {
    patchAll<LibImage[]>(qc, qk.libImages, (old) => pruneList(old, (i) => i.path, m, movedTo));
    patchAll<Hit[]>(qc, qk.libBig, (old) => pruneList(old, (h) => h.path, m, hitAt));
    patchAll<DupGroup[]>(qc, qk.libDups, (old) => pruneGroups(old, m));
    patchAll<{ hits: { path: string }[] }>(qc, ["library", "collEval"], (old) => pruneHits(old, m));
    patchAll<{ hits: { path: string }[] }>(qc, ["sem"], (old) => pruneHits(old, m));
    patchAll<DiskUsage>(qc, ["space", "dir"], (old) => patchUsage(old, m));
    patchAll<UsageDetail>(qc, ["space", "detail"], (old) => patchDetail(old, m));
  }

  // ---- what cannot be patched ----
  const arrivedPics = d.added.some(isPic) || d.renamed.some((r) => isPic(r.to) || !hasExt(r.to));
  const changedPics = d.modified.some(isPic);
  const leftPics = d.removed.some(maybeImage) || d.renamed.some((r) => maybeImage(r.from));
  // The timeline's base list lives in the backend (a load id): any picture coming, going or changing re-loads it.
  if (cached(qk.libTimelineBase) && (arrivedPics || changedPics || (leftPics && !opts.timelineCovered))) plan.soon.push(qk.libTimelineBase);
  // A new / changed picture's size, pixels and class come only from the backend.
  if (cached(qk.libImages) && (arrivedPics || changedPics)) plan.soon.push(qk.libImages);
  if (cached(qk.libDups) && (arrivedPics || changedPics)) plan.soon.push(qk.libDups);
  if (cached(qk.libBig) && (d.added.length > 0 || d.modified.length > 0)) plan.later.push(qk.libBig);

  // Space: only the folders on the way up from a change, and only those cached now; the removed folders themselves are not re-read.
  const touched = new Set(touchedAncestors(d));
  if (touched.size > 0) {
    for (const q of qc.getQueryCache().findAll({ queryKey: ["space"] })) {
      const p = q.queryKey[2];
      if (typeof p !== "string") continue;
      if (p === "" ? q.queryKey[1] === "dir" : touched.has(pathKey(p)) && !m.isGone(p)) plan.soon.push(q.queryKey);
    }
  }
  return plan;
}

type Listener = (d: FsDelta) => void;

/** One per QueryClient (= per window): every delta - the backend's and the app's own - goes through here. */
export class FsDeltaBus {
  private qc: QueryClient;
  private listeners = new Set<Listener>();
  private soon = new Map<string, QueryKey>();
  private later = new Map<string, QueryKey>();
  private soonT: ReturnType<typeof setTimeout> | undefined;
  private laterT: ReturnType<typeof setTimeout> | undefined;
  soonMs = 300;
  laterMs = 10_000;
  constructor(qc: QueryClient) { this.qc = qc; }

  /** Patch the caches, tell the listeners (popup rows, thumbnails, local lists), schedule the re-reads. */
  push(d: FsDelta, opts?: PatchOpts): DeltaPlan {
    const plan = patchCaches(this.qc, d, opts);
    for (const k of plan.soon) this.soon.set(JSON.stringify(k), k);
    for (const k of plan.later) this.later.set(JSON.stringify(k), k);
    if (this.soon.size && this.soonT === undefined) this.soonT = setTimeout(() => this.flushSoon(), this.soonMs);
    if (this.later.size && this.laterT === undefined) this.laterT = setTimeout(() => this.flushLater(), this.laterMs);
    for (const l of [...this.listeners]) l(d);
    return plan;
  }

  on(l: Listener): () => void {
    this.listeners.add(l);
    return () => { this.listeners.delete(l); };
  }

  flushSoon() {
    clearTimeout(this.soonT);
    this.soonT = undefined;
    const keys = [...this.soon.values()];
    this.soon.clear();
    for (const queryKey of keys) void this.qc.invalidateQueries({ queryKey, exact: true });
  }

  flushLater() {
    clearTimeout(this.laterT);
    this.laterT = undefined;
    const keys = [...this.later.values()];
    this.later.clear();
    for (const queryKey of keys) void this.qc.invalidateQueries({ queryKey, exact: true });
  }

  dispose() { clearTimeout(this.soonT); clearTimeout(this.laterT); this.listeners.clear(); }
}

const buses = new WeakMap<QueryClient, FsDeltaBus>();
export function fsDeltaBus(qc: QueryClient): FsDeltaBus {
  let b = buses.get(qc);
  if (!b) { b = new FsDeltaBus(qc); buses.set(qc, b); }
  return b;
}

/** The app's own change (a recycle / restore here): the same patch path as a backend delta, applied at once. */
export function pushLocalDelta(qc: QueryClient, d: FsDelta, opts?: PatchOpts): void {
  fsDeltaBus(qc).push(d, opts);
}
