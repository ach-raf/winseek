import { thumbStream } from "./thumbStream.ts";

/**
 * Picture versions and scroll reporting for the picture stream (thumbStream.ts). The per-request queue that lived
 * here (two tiers, a JS hold during flings, idle prefetch) is gone: the stream reads only what is near the viewport
 * and drops what scrolled away before it starts.
 */

// ---- thumbnail versions -----------------------------------------------------------------------------------------
// A picture changed on disk (an fs-delta `modified`): its tiles ask again under a new url (`&v=`), so the browser's
// cached bitmap is not reused; nothing else in the grid re-reads. Keyed by the lower-case path.

const vers = new Map<string, number>();
/** Subscribers by lower-case path: a bump wakes only the tiles that show one of the changed files. */
const verSubs = new Map<string, Set<() => void>>();
let verTick = 0;
const verKey = (p: string) => p.replace(/\//g, "\\").toLowerCase();

/** The path of a `wsicon` thumbnail url (its `p=` parameter); null for any other url (the mock's tiles). */
export function iconUrlPath(url: string): string | null {
  const i = url.indexOf("?");
  if (i < 0) return null;
  return new URLSearchParams(url.slice(i + 1)).get("p");
}

/** `url` asked again as re-read `r` (0 = the plain url). Only `http(s)` urls take a parameter. (`v=` is the file's
 *  mtime, see thumbUrl; `r=` is a change seen since that mtime was read.) */
export function withThumbVer(url: string, r: number): string {
  if (!r || !/^https?:/.test(url)) return url;
  return `${url}${url.includes("?") ? "&" : "?"}r=${r}`;
}

/** These pictures changed: the tiles showing one of them (and only those) re-request it. */
export function bumpThumbs(paths: readonly string[]) {
  if (paths.length === 0) return;
  verTick++;
  const woke = new Set<() => void>();
  for (const p of paths) {
    const k = verKey(p);
    vers.set(k, verTick);
    verSubs.get(k)?.forEach((f) => woke.add(f));
  }
  while (vers.size > 4000) vers.delete(vers.keys().next().value as string);
  woke.forEach((f) => f());
}
export function thumbVersion(path: string | undefined): number {
  return path ? vers.get(verKey(path)) ?? 0 : 0;
}
/** Wake `f` when `path` is bumped. */
export function subscribeThumbVer(path: string, f: () => void): () => void {
  const k = verKey(path);
  let s = verSubs.get(k);
  if (!s) { s = new Set(); verSubs.set(k, s); }
  s.add(f);
  return () => {
    const cur = verSubs.get(k);
    if (!cur) return;
    cur.delete(f);
    if (cur.size === 0) verSubs.delete(k);
  };
}
/** How many paths have a subscriber (tests). */
export const thumbVerSubscribedPaths = (): number => verSubs.size;

/** Every scrolling list reports its position here: a fast fling asks the picture stream for the instant level only
 *  until it settles. */
export function noteScroll(top: number) {
  thumbStream.noteScroll(top);
}
