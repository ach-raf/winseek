import type { Backend, ThumbBatch, ThumbWant } from "./types";

/**
 * The page half of the picture streamer (Rust: shell/thumbstream.rs). Built like a game's texture streamer:
 *
 * - Tiles register while mounted. Once per frame (at most every SEND_MS while scrolling) the manager measures them
 *   and sends Rust the tiles it wants, on screen first, centre out, then the ones just beyond. Rust replaces its
 *   list with it, so work for tiles that scrolled away is dropped before it starts.
 * - Two levels per tile, like mip levels: 1 = what the disk already has (Windows' thumbnail cache, the preview in a
 *   JPEG; about a millisecond), 2 = the picture decoded at the tile's size. During a fling only level 1 is asked
 *   for; level 2 follows when the scroll slows.
 * - Pixels arrive as raw RGBA in shared buffers. They are copied out at once (the buffer goes straight back to
 *   Rust), turned into ImageBitmaps and kept in a texture pool with a byte budget; the least recently shown
 *   pictures without a tile on screen are closed first. Scrolling back over what was just seen draws at once.
 */

/** Decoded pictures kept, in bytes (RGBA). About 1,600 screen-sized tiles. */
export const POOL_BYTES = 256 << 20;
/** Scroll speed (px per ms) above which only level 1 is asked for. */
export const FAST_PX_PER_MS = 2.5;
/** Stillness after the last scroll event that ends a fling. */
export const IDLE_MS = 110;
/** While scrolling, the wanted list is sent at most this often. */
export const SEND_MS = 40;

export interface TileReg {
  key: string;
  path: string;
  /** Device pixels. */
  w: number;
  h: number;
  /** Measured for on-screen order. */
  el: Element;
  show(bmp: ImageBitmap, level: number): void;
  /** No picture can be read (the tile shows its file icon). */
  fail(): void;
}

// State bits per key.
const HAVE_A = 1;
const HAVE_B = 2;
const NONE_A = 4;
const FAIL_B = 8;

interface PoolEntry { bmp: ImageBitmap; level: number; bytes: number }

interface WebViewSharedBufferEvent extends Event {
  additionalData?: { winseekThumbBuf?: number; gen?: number };
  getBuffer(): ArrayBuffer;
}
interface ChromeWebView {
  addEventListener(type: "sharedbufferreceived", cb: (e: WebViewSharedBufferEvent) => void): void;
  releaseBuffer?(b: ArrayBuffer): void;
}
declare global {
  interface Window { chrome?: { webview?: ChromeWebView } }
}

/** Order of the wanted list: on screen before just beyond, then nearest the viewport centre first. */
export function wantOrder(a: { vis: boolean; dist: number }, b: { vis: boolean; dist: number }): number {
  return a.vis === b.vis ? a.dist - b.dist : a.vis ? -1 : 1;
}

/** What a tile still needs given its state bits: level 1 unless something is already shown (or known missing),
 *  level 2 unless held, failed, or a fling is on. */
export function needOf(st: number, fast: boolean): number {
  let need = 0;
  if (!(st & (HAVE_A | HAVE_B | NONE_A))) need |= 1;
  if (!fast && !(st & (HAVE_B | FAIL_B))) need |= 2;
  return need;
}

/** Source rectangle that crops a `bw` x `bh` picture to cover a `cw` x `ch` box, centred. */
export function coverRect(bw: number, bh: number, cw: number, ch: number): [number, number, number, number] {
  const s = Math.max(cw / bw, ch / bh);
  const sw = cw / s, sh = ch / s;
  return [(bw - sw) / 2, (bh - sh) / 2, sw, sh];
}

/** Source and destination rectangles that fit a `bw` x `bh` picture whole inside a `cw` x `ch` box, centred. */
export function containRect(bw: number, bh: number, cw: number, ch: number): [number, number, number, number, number, number, number, number] {
  const s = Math.min(cw / bw, ch / bh);
  const dw = bw * s, dh = bh * s;
  return [0, 0, bw, bh, (cw - dw) / 2, (ch - dh) / 2, dw, dh];
}

export class ThumbStream {
  private backend: Backend | null = null;
  private gen = 0;
  /** Shared buffers by `${gen}:${index}`. */
  private bufs = new Map<string, ArrayBuffer>();
  /** Batches whose buffer has not been received yet. */
  private early: ThumbBatch[] = [];
  private regs = new Map<string, Set<TileReg>>();
  private pool = new Map<string, PoolEntry>();
  private poolBytes = 0;
  private state = new Map<string, number>();
  private fast = false;
  private lastTop = 0;
  private lastT = 0;
  private idle: ReturnType<typeof setTimeout> | undefined;
  private frame = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private lastSent = "";
  private lastSendT = 0;

  /** Bind to the window's backend and open the stream (once per backend). */
  attach(backend: Backend): void {
    if (this.backend === backend) return;
    this.backend = backend;
    window.chrome?.webview?.addEventListener("sharedbufferreceived", (e) => {
      const d = e.additionalData;
      if (d?.winseekThumbBuf === undefined || d.gen === undefined) return;
      this.bufs.set(`${d.gen}:${d.winseekThumbBuf}`, e.getBuffer());
      const ready = this.early.filter((b) => this.bufs.has(`${b.g}:${b.b}`));
      this.early = this.early.filter((b) => !this.bufs.has(`${b.g}:${b.b}`));
      for (const b of ready) this.onBatch(b);
    });
    addEventListener("resize", () => this.schedule());
    backend.thumbsOpen((b) => this.onBatch(b)).then((g) => {
      this.gen = g;
      for (const k of [...this.bufs.keys()]) if (!k.startsWith(`${g}:`)) this.bufs.delete(k);
      this.lastSent = "";
      this.schedule();
    }).catch((e) => console.error("winseek: picture stream did not open:", e));
  }

  /** A mounted tile. Draws at once from the pool when the picture is held. Returns the unregister function. */
  register(r: TileReg): () => void {
    let set = this.regs.get(r.key);
    if (!set) this.regs.set(r.key, (set = new Set()));
    set.add(r);
    const held = this.pool.get(r.key);
    if (held) {
      this.touch(r.key, held);
      r.show(held.bmp, held.level);
    } else if ((this.state.get(r.key) ?? 0) & FAIL_B && !((this.state.get(r.key) ?? 0) & (HAVE_A | HAVE_B))) {
      r.fail();
    }
    this.schedule();
    return () => {
      const s = this.regs.get(r.key);
      s?.delete(r);
      if (s && !s.size) this.regs.delete(r.key);
      this.schedule();
    };
  }

  /** A scroll position sample from any grid: a fast fling asks for level 1 only until it settles. */
  noteScroll(top: number, now: number = performance.now()): void {
    const dt = now - this.lastT;
    if (this.lastT > 0 && dt > 0 && dt < 250) {
      const fast = Math.abs(top - this.lastTop) / dt > FAST_PX_PER_MS;
      if (fast !== this.fast) this.fast = fast;
    }
    this.lastTop = top;
    this.lastT = now;
    clearTimeout(this.idle);
    this.idle = setTimeout(() => { this.lastT = 0; if (this.fast) { this.fast = false; this.schedule(); } }, IDLE_MS);
    this.schedule();
  }

  /** Recompute the wanted list on the next frame (rate-limited while scrolling). */
  schedule(): void {
    if (this.frame || this.timer) return;
    const wait = SEND_MS - (performance.now() - this.lastSendT);
    const run = () => { this.frame = requestAnimationFrame(() => { this.frame = 0; this.sendWants(); }); };
    if (wait > 0) this.timer = setTimeout(() => { this.timer = undefined; run(); }, wait);
    else run();
  }

  private sendWants(): void {
    if (!this.gen || !this.backend) return;
    const vw = innerWidth, vh = innerHeight;
    const list: { vis: boolean; dist: number; w: ThumbWant }[] = [];
    for (const [key, set] of this.regs) {
      const st = this.state.get(key) ?? 0;
      if (st & (HAVE_B | FAIL_B)) continue; // nothing more to read for it
      const r0 = set.values().next().value as TileReg;
      const r = r0.el.getBoundingClientRect();
      if (!r.width) continue;
      if (r.bottom < -vh || r.top > 2 * vh) continue; // mounted far off (overscan): not yet
      const vis = r.bottom > 0 && r.top < vh && r.right > 0 && r.left < vw;
      const dist = Math.hypot(r.left + r.width / 2 - vw / 2, r.top + r.height / 2 - vh / 2);
      // need 0 still goes: it keeps a level-2 read already under way wanted through a fling.
      list.push({ vis, dist, w: [key, r0.path, r0.w, r0.h, needOf(st, this.fast), vis ? 1 : 0] });
    }
    list.sort(wantOrder);
    const items = list.map((x) => x.w);
    const msg = JSON.stringify(items);
    if (msg === this.lastSent) return;
    this.lastSent = msg;
    this.lastSendT = performance.now();
    this.backend.thumbsWant(this.gen, items).catch((e) => console.error("winseek: picture stream want failed:", e));
  }

  private onBatch(b: ThumbBatch): void {
    const buf = b.data ?? this.bufs.get(`${b.g}:${b.b}`);
    if (!buf) { this.early.push(b); return; }
    for (const it of b.items) {
      const st = this.state.get(it.k) ?? 0;
      if (it.f) {
        this.state.set(it.k, st | (it.l === 1 ? NONE_A : FAIL_B));
        if (it.l === 2 && !(st & HAVE_A)) for (const r of this.regs.get(it.k) ?? []) r.fail();
        continue;
      }
      if (it.l === 1 && st & HAVE_B) continue;
      this.state.set(it.k, st | (it.l === 1 ? HAVE_A : HAVE_B));
      // Copied out now: the buffer goes back to Rust right after this loop.
      const px = new Uint8ClampedArray(buf, it.o, it.n).slice();
      createImageBitmap(new ImageData(px, it.w, it.h))
        .then((bmp) => this.put(it.k, bmp, it.l))
        .catch((e) => console.error("winseek: picture decode failed:", it.k, e));
    }
    if (!b.data) this.backend?.thumbsAck(b.g, b.b).catch((e) => console.error("winseek: picture stream ack failed:", e));
    this.schedule();
  }

  private put(key: string, bmp: ImageBitmap, level: number): void {
    const old = this.pool.get(key);
    if (old && old.level >= level) { bmp.close(); return; }
    if (old) { old.bmp.close(); this.poolBytes -= old.bytes; this.pool.delete(key); }
    const e = { bmp, level, bytes: bmp.width * bmp.height * 4 };
    this.pool.set(key, e);
    this.poolBytes += e.bytes;
    for (const r of this.regs.get(key) ?? []) r.show(bmp, level);
    this.evict();
  }

  private touch(key: string, e: PoolEntry): void {
    this.pool.delete(key);
    this.pool.set(key, e);
  }

  /** Close the least recently shown pictures that no mounted tile shows until the pool fits its budget. */
  private evict(): void {
    if (this.poolBytes <= POOL_BYTES) return;
    for (const [key, e] of this.pool) {
      if (this.poolBytes <= POOL_BYTES) break;
      if (this.regs.has(key)) continue;
      e.bmp.close();
      this.poolBytes -= e.bytes;
      this.pool.delete(key);
      this.state.delete(key);
    }
  }

  /** Pictures held and their bytes (tests, diagnostics). */
  get held(): { count: number; bytes: number } { return { count: this.pool.size, bytes: this.poolBytes }; }
}

/** The page's one stream. */
export const thumbStream = new ThumbStream();
