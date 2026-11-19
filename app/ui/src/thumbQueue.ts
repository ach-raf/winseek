/**
 * Thumbnail request scheduler for tile grids.
 *
 * Pointing every <img> at the `wsicon:` URL queues one backend job per tile the instant it mounts; a fast scroll
 * leaves hundreds of stale jobs in front of the tiles that are actually on screen, and nothing can recall them.
 * Here the page holds the queue instead: only `MAX_ACTIVE` requests are ever in flight, the newest request is
 * served first (the tile that mounted last is the one under the viewport), and a tile that unmounts withdraws its
 * request (queued -> dropped; in flight -> aborted by clearing the loader's src).
 */

export const MAX_ACTIVE = 4;
/** Waiting requests kept; the oldest beyond this are dropped (their tiles are long scrolled away). */
export const MAX_WAITING = 300;
/** Scroll speed (px per ms) above which new requests are held, and below which they resume at once. */
export const FAST_PX_PER_MS = 1.4;
export const SLOW_PX_PER_MS = 0.5;
/** Stillness after the last scroll event that releases a hold. */
export const IDLE_MS = 90;
/** Loaded urls remembered as "draw at once" - about what the browser's decoded-image cache can really hold. */
export const MAX_DONE = 1500;

/** Starts a load; returns the abort function. `done(ok)` is called once when the load ends. */
export type Loader = (url: string, done: (ok: boolean) => void) => () => void;

interface Entry {
  url: string;
  cb: (ok: boolean) => void;
  abort?: () => void;
}

export class ThumbQueue {
  private waiting: Entry[] = [];
  private active = new Set<Entry>();
  /** URLs known to be in the browser's cache by now (loaded OK): the next tile for one resolves at once. */
  private done = new Set<string>();
  private load: Loader;
  private maxActive: number;
  private maxWaiting: number;
  constructor(load: Loader, maxActive = MAX_ACTIVE, maxWaiting = MAX_WAITING) {
    this.load = load;
    this.maxActive = maxActive;
    this.maxWaiting = maxWaiting;
  }

  /** True when `url` finished loading earlier (the image can be drawn without waiting). */
  has(url: string): boolean { return this.done.has(url); }

  private held = false;
  private lastTop = 0;
  private lastT = 0;
  private idle: ReturnType<typeof setTimeout> | undefined;
  /** True while a fast scroll keeps new requests back. */
  get isHeld(): boolean { return this.held; }
  /** Hold (or release) new requests; in-flight ones finish, tiles that unmount still withdraw. */
  setHold(h: boolean) {
    if (this.held === h) return;
    this.held = h;
    if (!h) this.pump();
  }
  /**
   * Scroll sample (`top` in px, `now` in ms). A fast fling holds new requests (nothing visible survives it, so
   * every job started would be wasted decode + memory); slower movement or 90 ms of stillness releases them and
   * the newest tiles (the ones now on screen) load first.
   */
  noteScroll(top: number, now: number = performance.now()) {
    const dt = now - this.lastT;
    if (this.lastT > 0 && dt > 0 && dt < 250) {
      const v = Math.abs(top - this.lastTop) / dt;
      if (v > FAST_PX_PER_MS) this.setHold(true);
      else if (v < SLOW_PX_PER_MS) this.setHold(false);
    }
    this.lastTop = top;
    this.lastT = now;
    clearTimeout(this.idle);
    this.idle = setTimeout(() => { this.lastT = 0; this.setHold(false); }, IDLE_MS);
  }

  /** Request `url`; `cb(ok)` fires when it has loaded. Returns the cancel function. */
  request(url: string, cb: (ok: boolean) => void): () => void {
    if (this.done.has(url)) { cb(true); return () => {}; }
    const e: Entry = { url, cb };
    this.waiting.push(e);
    while (this.waiting.length > this.maxWaiting) this.waiting.shift(); // oldest = scrolled past
    this.pump();
    return () => this.cancel(e);
  }

  get pending(): number { return this.waiting.length + this.active.size; }

  /** Drop every url `gone` matches: waiting requests are withdrawn (their tiles are leaving), and a finished one no
   *  longer counts as cached (a file put back later loads fresh). In-flight loads finish on their own. */
  forget(gone: (url: string) => boolean) {
    this.waiting = this.waiting.filter((e) => !gone(e.url));
    for (const u of [...this.done]) if (gone(u)) this.done.delete(u);
  }

  private cancel(e: Entry) {
    const i = this.waiting.indexOf(e);
    if (i >= 0) { this.waiting.splice(i, 1); return; }
    if (this.active.delete(e)) { e.abort?.(); this.pump(); }
  }

  private pump() {
    while (!this.held && this.active.size < this.maxActive && this.waiting.length) {
      const e = this.waiting.pop()!; // newest first
      this.active.add(e);
      let finished = false;
      const end = (ok: boolean) => {
        if (finished || !this.active.has(e)) return;
        finished = true;
        this.active.delete(e);
        if (ok) {
          this.done.add(e.url);
          if (this.done.size > MAX_DONE) this.done.delete(this.done.values().next().value as string);
        }
        e.cb(ok);
        this.pump();
      };
      e.abort = this.load(e.url, end);
    }
  }
}

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

const imageLoader: Loader = (url, done) => {
  const img = new Image();
  img.decoding = "async";
  img.onload = () => done(true);
  img.onerror = () => done(false);
  img.src = url;
  return () => { img.onload = null; img.onerror = null; img.src = ""; };
};

/** The page's queue for sharp thumbnails: an Image element per request (the decoded bitmap stays in the browser's
 *  cache). Held during a fling. */
export const thumbs = new ThumbQueue(imageLoader);

/** Placeholder lane: the tiny (32 px) tier every tile shows first, blurred. Never held — a fling still paints
 *  every tile it passes with its colours, never an empty box — and twice as wide as the sharp lane. */
export const LOW_ACTIVE = 8;
export const lowThumbs = new ThumbQueue(imageLoader, LOW_ACTIVE, MAX_WAITING);

/**
 * One tile's two tiers. The placeholder is asked for first (fast lane); the sharp picture is queued once the
 * placeholder has arrived (or failed), behind the hold — so a fling paints placeholders and nothing else, and when
 * it settles the newest tiles sharpen first. A sharp picture the page already has skips the placeholder.
 * Returns the cancel function (withdraws both).
 */
export function requestThumb(
  sharp: string, low: string | null,
  onLow: () => void, onSharp: (ok: boolean) => void,
  q: ThumbQueue = thumbs, lq: ThumbQueue = lowThumbs,
): () => void {
  if (q.has(sharp) || !low || low === sharp) return q.request(sharp, onSharp);
  let cancelSharp: (() => void) | null = null;
  let dead = false;
  const cancelLow = lq.request(low, (ok) => {
    if (dead) return;
    if (ok) onLow();
    cancelSharp = q.request(sharp, onSharp);
  });
  return () => { dead = true; cancelLow(); cancelSharp?.(); };
}

/** Every scrolling list reports its position here: a fast fling holds sharp thumbnail requests, and any scroll stops
 *  the idle prefetch (its rows were the old window; the next settle queues the new one). */
export function noteScroll(top: number) {
  thumbs.noteScroll(top);
  stopWarm();
}

// ---- idle prefetch ----------------------------------------------------------------------------------------------
// Rows just beyond the viewport are rendered into the backend's caches (`w=1`: the backend answers 204 and keeps the
// bytes, the page decodes nothing), so by the time they scroll in they come off the disk cache instead of the shell.
// Runs only while no real thumbnail is waiting and no fling is in progress; two at a time.

const warmed = new Set<string>();
const warmQ: string[] = [];
const warmLive = new Map<string, AbortController>();
const WARM_PARALLEL = 2;
const WARM_MAX = 96;

/** Drop the prefetch queue and abort the prefetches in flight (a scroll moved the window). An aborted url may be
 *  warmed again later. */
function stopWarm() {
  warmQ.length = 0;
  for (const [u, c] of warmLive) { c.abort(); warmed.delete(u); }
  warmLive.clear();
}

function pumpWarm() {
  while (warmLive.size < WARM_PARALLEL && warmQ.length && !thumbs.isHeld && thumbs.pending === 0 && lowThumbs.pending === 0) {
    const url = warmQ.shift()!;
    const c = new AbortController();
    warmLive.set(url, c);
    fetch(url + "&w=1", { cache: "no-store", signal: c.signal }).catch(() => {}).finally(() => {
      if (warmLive.get(url) === c) warmLive.delete(url);
      pumpWarm();
    });
  }
}

/** Queue thumbnail urls for background rendering (nearest first); anything already seen or queued is skipped. */
export function warm(urls: string[]) {
  if (thumbs.isHeld) return;
  warmQ.length = 0; // the newest window replaces the old one: its rows were scrolled past
  for (const u of urls) {
    if (warmQ.length >= WARM_MAX) break;
    if (warmed.has(u) || thumbs.has(u)) continue;
    warmed.add(u);
    if (warmed.size > 8000) warmed.delete(warmed.values().next().value as string);
    warmQ.push(u);
  }
  pumpWarm();
}
