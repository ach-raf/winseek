/**
 * Thumbnail pixel sizes. A tile crops its picture with `object-fit: cover`, so the thumbnail's long side must be
 * about 1.4x the tile's (a 4:3 picture shows only its short side across a square tile), times the screen's pixel
 * ratio. The result snaps UP to a few buckets: every size is its own backend render and disk-cache entry, so a
 * continuous range would turn one library into several caches.
 */
const BUCKETS = [64, 96, 128, 192, 256, 384, 512] as const;
const COVER = 1.4;

export function thumbPx(cssW: number, cssH: number = cssW, dpr: number = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1): number {
  const need = Math.max(cssW, cssH) * COVER * Math.max(1, dpr);
  return BUCKETS.find((b) => b >= need) ?? BUCKETS[BUCKETS.length - 1]!;
}
