import type { DiskUsage, DupGroup, FsDelta, Hit, UsageDetail } from "./types.ts";

/**
 * File-system deltas (the backend's `fs-delta` event, or one the app builds for its own recycle): pure
 * matching and list patching. Every cached list is patched in place from here, so a delete / rename made
 * anywhere (Explorer included) leaves the screen without a whole-list re-read.
 */

/** A delta with only the given parts (the rest empty). */
export function fsDelta(part: Partial<FsDelta>): FsDelta {
  return { removed: [], renamed: [], added: [], modified: [], dirs: [], overflow: false, ...part };
}

/** Comparison key of a Windows path: `/` -> `\`, lower case, no trailing separator (`C:\` -> `c:`). */
export function pathKey(p: string): string {
  let k = p.replace(/\//g, "\\").toLowerCase();
  while (k.length > 1 && k.endsWith("\\")) k = k.slice(0, -1);
  return k;
}

/** Parent of a key (`c:\a\b` -> `c:\a`, `c:\a` -> `c:`); null at a drive root / bare name. */
function parentKey(k: string): string | null {
  const i = k.lastIndexOf("\\");
  return i > 0 ? k.slice(0, i) : null;
}

/** The last path segment. */
export function baseName(p: string): string {
  const t = p.replace(/\//g, "\\").replace(/\\+$/, "");
  return t.slice(t.lastIndexOf("\\") + 1);
}

export interface Matcher {
  /** Nothing is gone and nothing renamed: every patch is a no-op. */
  readonly idle: boolean;
  /** The path no longer exists: removed, under a removed folder, or the old side of a rename (exact or
   *  under a renamed folder). A path re-created in the same batch (added / a rename's new side) is not gone. */
  isGone(path: string): boolean;
  /** The path after this batch's renames (applied in order, so chains and swaps resolve); the same string when untouched. */
  rewrite(path: string): string;
}

export function makeMatcher(d: FsDelta): Matcher {
  const gone = new Set<string>();
  for (const p of d.removed) gone.add(pathKey(p));
  for (const r of d.renamed) gone.add(pathKey(r.from));
  const alive = new Set<string>();
  for (const p of d.added) alive.add(pathKey(p));
  for (const r of d.renamed) alive.add(pathKey(r.to));
  const ren = d.renamed.map((r) => ({ from: pathKey(r.from), to: r.to.replace(/\//g, "\\").replace(/(?<!^[A-Za-z]:)\\+$/, "") }));
  return {
    idle: gone.size === 0 && ren.length === 0,
    isGone(path) {
      if (gone.size === 0) return false;
      const k = pathKey(path);
      if (alive.has(k)) return false;
      for (let c: string | null = k; c; c = parentKey(c)) if (gone.has(c)) return true;
      return false;
    },
    rewrite(path) {
      if (ren.length === 0) return path;
      let cur = path.replace(/\//g, "\\");
      let moved = false;
      for (const r of ren) {
        const k = pathKey(cur);
        if (k === r.from) { cur = r.to; moved = true; }
        else if (k.startsWith(r.from + "\\")) {
          // the tail keeps its case; a key whose length differs (rare Unicode folding) falls back to the key's tail
          const tail = (k.length === cur.replace(/\\+$/, "").length ? cur : k).slice(r.from.length);
          cur = r.to.replace(/\\+$/, "") + tail;
          moved = true;
        }
      }
      return moved ? cur : path;
    },
  };
}

const asMatcher = (x: FsDelta | Matcher): Matcher => ("isGone" in x ? x : makeMatcher(x));

/** Every folder whose contents (or roll-up) this delta changes, as `pathKey`s: all ancestors of every
 *  removed / renamed (both sides) / added / modified path, plus `dirs` and their ancestors. Deduped. */
export function touchedAncestors(d: FsDelta): string[] {
  const out = new Set<string>();
  const up = (k: string | null) => { for (let c = k; c && !out.has(c); c = parentKey(c)) out.add(c); };
  const paths = [...d.removed, ...d.added, ...d.modified, ...d.renamed.flatMap((r) => [r.from, r.to])];
  for (const p of paths) up(parentKey(pathKey(p)));
  for (const p of d.dirs) up(pathKey(p));
  return [...out];
}

/** `items` without the gone ones and with renamed paths rewritten (`withPath` builds the moved item; without it a
 *  renamed item is dropped). The same array back when nothing changed, so a setQueryData can skip the write. */
export function pruneList<T>(items: readonly T[], getPath: (t: T) => string, delta: FsDelta | Matcher, withPath?: (t: T, path: string) => T): T[] {
  const m = asMatcher(delta);
  if (m.idle) return items as T[];
  let changed = false;
  const out: T[] = [];
  for (const it of items) {
    const p = getPath(it);
    const np = m.rewrite(p);
    if (np !== p) { changed = true; if (withPath) out.push(withPath(it, np)); continue; }
    if (m.isGone(p)) { changed = true; continue; }
    out.push(it);
  }
  return changed ? out : (items as T[]);
}

/** A hit moved to `path` (its name follows). */
export const hitAt = <H extends { path: string; name: string }>(h: H, path: string): H => ({ ...h, path, name: baseName(path) });
/** Any `{ path }` record moved to `path`. */
export const movedTo = <H extends { path: string }>(h: H, path: string): H => ({ ...h, path });

/** Duplicate groups: removed members leave, renamed ones move; a group with fewer than two left is dropped. */
export function pruneGroups(groups: readonly DupGroup[], delta: FsDelta | Matcher): DupGroup[] {
  const m = asMatcher(delta);
  if (m.idle) return groups as DupGroup[];
  let changed = false;
  const out: DupGroup[] = [];
  for (const g of groups) {
    const members = pruneList(g.members, (x) => x.path, m, movedTo);
    if (members === g.members) { out.push(g); continue; }
    changed = true;
    if (members.length < 2) continue;
    const sizes = members.map((x) => x.size);
    out.push({ ...g, members, reclaimable: sizes.reduce((a, b) => a + b, 0) - Math.max(...sizes) });
  }
  return changed ? out : (groups as DupGroup[]);
}

/** Anything with a `hits: { path }[]` list (meaning answers, collection evaluations). */
export function pruneHits<R extends { hits: readonly { path: string }[] }>(r: R, delta: FsDelta | Matcher): R {
  const hits = pruneList(r.hits, (h) => h.path, delta, movedTo);
  return hits === r.hits ? r : { ...r, hits };
}

/** One Space folder roll-up: gone children leave (their bytes / counts come off the totals at once, the
 *  ancestors' own roll-ups are re-read); renamed children move. */
export function patchUsage(d: DiskUsage, delta: FsDelta | Matcher): DiskUsage {
  const m = asMatcher(delta);
  if (m.idle) return d;
  let size = d.size, files = d.files, folders = d.folders, changed = false;
  const entries: DiskUsage["entries"] = [];
  for (const e of d.entries) {
    const np = m.rewrite(e.path);
    if (np !== e.path) { changed = true; entries.push(hitAt(e, np)); continue; }
    if (m.isGone(e.path)) {
      changed = true;
      size -= e.size;
      files -= e.is_dir ? e.files : 1;
      folders -= e.is_dir ? e.folders + 1 : 0;
      continue;
    }
    entries.push(e);
  }
  if (!changed) return d;
  return { ...d, entries, size: Math.max(0, size), files: Math.max(0, files), folders: Math.max(0, folders) };
}

/** One Space folder's detail: the biggest-files list loses gone files. */
export function patchDetail(d: UsageDetail, delta: FsDelta | Matcher): UsageDetail {
  const biggest = pruneList(d.biggest, (h) => h.path, delta, hitAt);
  return biggest === d.biggest ? d : { ...d, biggest };
}

/**
 * The popup's sparse result store (indexes up to the result count; only loaded pages hold rows): gone rows
 * leave and the rows after them shift up; renamed rows move in place. `removed` = the old indices that left.
 */
export function pruneRows(rows: readonly (Hit | undefined)[], delta: FsDelta | Matcher): { rows: (Hit | undefined)[]; removed: number[]; changed: boolean } {
  const m = asMatcher(delta);
  if (m.idle) return { rows: rows as (Hit | undefined)[], removed: [], changed: false };
  const out: (Hit | undefined)[] = [];
  const removed: number[] = [];
  let changed = false;
  for (const k in rows) {
    const j = Number(k);
    const h = rows[j];
    if (!h) continue;
    const np = m.rewrite(h.path);
    if (np !== h.path) { changed = true; out[j - removed.length] = hitAt(h, np); continue; }
    if (m.isGone(h.path)) { changed = true; removed.push(j); continue; }
    out[j - removed.length] = h;
  }
  return changed ? { rows: out, removed, changed } : { rows: rows as (Hit | undefined)[], removed, changed };
}

/** Where a selection at `sel` lands after `removed` (ascending old indices) left: it keeps pointing at the
 *  same row, or — when that row left — at the one that took its place (the next), else the new last one. */
export function shiftIndex(sel: number, removed: readonly number[], newTotal: number): number {
  let below = 0;
  for (const r of removed) if (r < sel) below++;
  return Math.max(0, Math.min(sel - below, newTotal - 1));
}

const IMG_EXT = new Set(["jpg", "jpeg", "jfif", "png", "gif", "webp", "bmp", "heic", "heif", "avif", "tif", "tiff", "dng", "cr2", "nef", "arw", "raf", "orf", "rw2"]);
/** The path could be a picture (or a folder that holds pictures: no extension). */
export function maybeImage(p: string): boolean {
  const n = baseName(p);
  const i = n.lastIndexOf(".");
  return i < 0 || IMG_EXT.has(n.slice(i + 1).toLowerCase());
}

/** Something new could have entered a list (added, or arrived by a rename) - the lists that cannot be patched for that re-read. */
export function hasArrivals(d: FsDelta): boolean {
  return d.added.length > 0 || d.renamed.length > 0;
}
