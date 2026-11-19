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

/** The page's queue: an Image element per request (the decoded bitmap stays in the browser's cache). */
export const thumbs = new ThumbQueue((url, done) => {
  const img = new Image();
  img.decoding = "async";
  img.onload = () => done(true);
  img.onerror = () => done(false);
  img.src = url;
  return () => { img.onload = null; img.onerror = null; img.src = ""; };
});

/** Every scrolling list reports its position here: a fast fling holds thumbnail requests and idle prefetch. */
export function noteScroll(top: number) {
  thumbs.noteScroll(top);
  if (thumbs.isHeld) warmQ.length = 0;
}

// ---- idle prefetch ----------------------------------------------------------------------------------------------
// Rows just beyond the viewport are rendered into the backend's caches (`w=1`: the backend answers 204 and keeps the
// bytes, the page decodes nothing), so by the time they scroll in they come off the disk cache instead of the shell.
// Runs only while no real thumbnail is waiting and no fling is in progress; two at a time.

const warmed = new Set<string>();
const warmQ: string[] = [];
let warmActive = 0;
const WARM_PARALLEL = 2;
const WARM_MAX = 96;

function pumpWarm() {
  while (warmActive < WARM_PARALLEL && warmQ.length && !thumbs.isHeld && thumbs.pending === 0) {
    const url = warmQ.shift()!;
    warmActive++;
    fetch(url + "&w=1", { cache: "no-store" }).catch(() => {}).finally(() => { warmActive--; pumpWarm(); });
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
