/**
 * Thumbnail request scheduler for tile grids.
 *
 * Pointing every <img> at the `wsicon:` URL queues one backend job per tile the instant it mounts; a fast scroll
 * leaves hundreds of stale jobs in front of the tiles that are actually on screen, and nothing can recall them.
 * Here the page holds the queue instead: only `MAX_ACTIVE` requests are ever in flight, the newest request is
 * served first (the tile that mounted last is the one under the viewport), and a tile that unmounts withdraws its
 * request (queued -> dropped; in flight -> aborted by clearing the loader's src).
 */

export const MAX_ACTIVE = 6;
/** Waiting requests kept; the oldest beyond this are dropped (their tiles are long scrolled away). */
export const MAX_WAITING = 400;

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
    while (this.active.size < this.maxActive && this.waiting.length) {
      const e = this.waiting.pop()!; // newest first
      this.active.add(e);
      let finished = false;
      const end = (ok: boolean) => {
        if (finished || !this.active.has(e)) return;
        finished = true;
        this.active.delete(e);
        if (ok) {
          this.done.add(e.url);
          if (this.done.size > 6000) this.done.delete(this.done.values().next().value as string);
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
