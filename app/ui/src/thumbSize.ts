import type { Backend } from "./types";

/**
 * Thumbnail pixel sizes. A tile crops its picture with `object-fit: cover`, so the thumbnail's long side must be
 * about 1.4x the tile's (a 4:3 picture shows only its short side across a square tile), times the screen's pixel
 * ratio. The result snaps UP to a few buckets: every size is its own backend render and disk-cache entry, so a
 * continuous range would turn one library into several caches.
 */
const BUCKETS = [64, 96, 128, 192, 256, 384, 512] as const;
const COVER = 1.4;
/** The placeholder tier every tile draws first (blurred): near-free for the backend, a few hundred bytes. Not a
 *  bucket of thumbPx — nothing is ever shown sharp at this size. */
export const LOW_PX = 32;

export function thumbPx(cssW: number, cssH: number = cssW, dpr: number = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1): number {
  const need = Math.max(cssW, cssH) * COVER * Math.max(1, dpr);
  return BUCKETS.find((b) => b >= need) ?? BUCKETS[BUCKETS.length - 1]!;
}

/**
 * A picture's thumbnail url at `px`. With the file's `mtime` known it rides along as `v=`: the url then names one
 * exact version of the file, so the backend can serve it as immutable and an edited file asks under a new url.
 */
export function thumbUrl(backend: Pick<Backend, "iconUrl">, path: string, px: number, mtime?: number | null): string | null {
  const u = backend.iconUrl(path, false, true, px);
  if (!u) return null;
  return mtime && /^https?:/.test(u) ? `${u}${u.includes("?") ? "&" : "?"}v=${Math.floor(mtime)}` : u;
}

/** Both tiers of a tile `cssW` x `cssH` css px. */
export function thumbPair(backend: Pick<Backend, "iconUrl">, path: string, cssW: number, cssH: number, mtime?: number | null): { src: string | null; low: string | null } {
  const src = thumbUrl(backend, path, thumbPx(cssW, cssH), mtime);
  if (!src) return { src: null, low: null };
  const low = thumbUrl(backend, path, LOW_PX, mtime);
  return { src, low: low === src ? null : low };
}
