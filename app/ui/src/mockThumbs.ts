import type { Backend, ThumbBatch, ThumbItem, ThumbWant } from "./types";

/**
 * The picture streamer for the browser mock: same contract as shell/thumbstream.rs (wanted list replaces the old
 * one, on screen level 1 then level 2 then the ring, results for tiles no longer wanted are dropped), with
 * simulated read times and synthetic pixels. Pixels travel inline (`data`) instead of shared buffers.
 */

const WORKERS = 4;
/** Simulated read times (ms): the instant level, and a decode at tile size. */
const A_MS = 1;
const B_MS = 28;
const COALESCE_MS = 6;

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

/** A diagonal two-colour gradient picked from the path (level 1 at a quarter size, like a small preview). */
function pixels(path: string, w: number, h: number): Uint8ClampedArray {
  const x = hash(path);
  const c1 = [x & 255, (x >> 8) & 255, (x >> 16) & 255], c2 = [255 - c1[0]!, (x >> 4) & 255, 255 - c1[2]!];
  const px = new Uint8ClampedArray(w * h * 4);
  for (let j = 0; j < h; j++) {
    for (let i = 0; i < w; i++) {
      const t = (i / w + j / h) / 2, o = (j * w + i) * 4;
      px[o] = c1[0]! + (c2[0]! - c1[0]!) * t;
      px[o + 1] = c1[1]! + (c2[1]! - c1[1]!) * t;
      px[o + 2] = c1[2]! + (c2[2]! - c1[2]!) * t;
      px[o + 3] = 255;
    }
  }
  return px;
}

export function mockThumbStream(): Pick<Backend, "thumbsOpen" | "thumbsWant" | "thumbsAck"> {
  let gen = 0;
  let cb: ((b: ThumbBatch) => void) | null = null;
  let wants: ThumbWant[] = [];
  let wanted = new Set<string>();
  const pending = new Set<string>();
  let out: { it: ThumbItem; px: Uint8ClampedArray | null }[] = [];
  let flushT: ReturnType<typeof setTimeout> | undefined;
  let busy = 0;

  const pick = (): { w: ThumbWant; level: 1 | 2 } | null => {
    for (const [vis, level] of [[1, 1], [1, 2], [0, 1], [0, 2]] as const) {
      const w = wants.find((x) => x[5] === vis && x[4] & level && !pending.has(`${x[0]}#${level}`)
        && !(level === 1 && x[4] & 2 && pending.has(`${x[0]}#2`)));
      if (w) return { w, level };
    }
    return null;
  };

  const flush = () => {
    flushT = undefined;
    if (!out.length || !cb) return;
    const size = out.reduce((n, o) => n + (o.px?.length ?? 0), 0);
    const data = new Uint8ClampedArray(size);
    let off = 0;
    const items: ThumbItem[] = [];
    for (const o of out) {
      if (o.px) { data.set(o.px, off); items.push({ ...o.it, o: off, n: o.px.length }); off += o.px.length; } else items.push(o.it);
      pending.delete(`${o.it.k}#${o.it.l}`);
    }
    out = [];
    cb({ g: gen, b: 0, items, data: data.buffer });
  };

  const pump = () => {
    while (busy < WORKERS) {
      const t = pick();
      if (!t) return;
      const [key, path, w, h] = t.w;
      pending.add(`${key}#${t.level}`);
      busy++;
      setTimeout(() => {
        busy--;
        if (!wanted.has(key)) { pending.delete(`${key}#${t.level}`); pump(); return; }
        // Most pictures have an instant level; one in six does not (no cache entry, no embedded preview).
        const noA = t.level === 1 && hash(path) % 6 === 0;
        const pw = t.level === 1 ? Math.max(1, w >> 2) : w, ph = t.level === 1 ? Math.max(1, h >> 2) : h;
        out.push(noA
          ? { it: { k: key, l: 1, w: 0, h: 0, o: 0, n: 0, f: 1 }, px: null }
          : { it: { k: key, l: t.level, w: pw, h: ph, o: 0, n: 0, f: 0 }, px: pixels(path, pw, ph) });
        flushT ??= setTimeout(flush, COALESCE_MS);
        pump();
      }, t.level === 1 ? A_MS : B_MS);
    }
  };

  return {
    async thumbsOpen(onBatch) { cb = onBatch; return ++gen; },
    async thumbsWant(g, items) {
      if (g !== gen) return;
      wants = items;
      wanted = new Set(items.map((x) => x[0]));
      out = out.filter((o) => { const keep = wanted.has(o.it.k); if (!keep) pending.delete(`${o.it.k}#${o.it.l}`); return keep; });
      pump();
    },
    async thumbsAck() { /* pixels travel inline in the mock */ },
  };
}
