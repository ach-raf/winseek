// Synthetic photo library for scroll checks at scale (`?view=library&photos=1000000` in the browser mock).
// Columns in typed arrays (about 13 bytes per photo) and the path made on demand from the index, so the mock
// backend itself stays small enough that a heap reading of the page is about the page.

export type SynthClass = "photo" | "screenshot" | "graphic";
const CLASSES: SynthClass[] = ["photo", "screenshot", "graphic"];

export interface Synth {
  n: number;
  mtime: Float64Array;
  size: Uint32Array;
  /** 0 photo, 1 screenshot, 2 graphic */
  cls: Uint8Array;
  path(i: number): string;
  className(i: number): SynthClass;
}

const DIR = "C:\\Users\\User\\Pictures\\Synth";
const SPAN_MS = 12 * 365 * 86_400_000;

const mix = (x: number): number => {
  x = Math.imul(x ^ (x >>> 16), 0x45d9f3b);
  x = Math.imul(x ^ (x >>> 16), 0x45d9f3b);
  return ((x ^ (x >>> 16)) >>> 0) / 4294967296;
};

/** Newest first by index: photo i is a little older than photo i-1. */
export function buildSynth(n: number, now: number): Synth {
  const mtime = new Float64Array(n), size = new Uint32Array(n), cls = new Uint8Array(n);
  const step = SPAN_MS / Math.max(1, n);
  for (let i = 0; i < n; i++) {
    mtime[i] = Math.floor(now - i * step);
    size[i] = 200_000 + Math.floor(mix(i) * 5_000_000);
    const r = mix(i + 0x9e3779b9);
    cls[i] = r < 0.1 ? 2 : r < 0.25 ? 1 : 0;
  }
  return {
    n, mtime, size, cls,
    path: (i) => `${DIR}\\${new Date(mtime[i] ?? 0).getFullYear()}\\${cls[i] === 1 ? "Screenshot" : "IMG"}_${String(i).padStart(7, "0")}.${cls[i] === 1 ? "png" : "jpg"}`,
    className: (i) => CLASSES[cls[i] ?? 0] ?? "photo",
  };
}

/** The index a synthetic path was made from, or -1. */
export function synthIndex(path: string): number {
  if (!path.startsWith(DIR)) return -1;
  const m = /_(\d{7})\.\w+$/.exec(path);
  return m ? Number(m[1]) : -1;
}
