import { useCallback, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { containRect, coverRect, thumbStream } from "./thumbStream";
import { subscribeThumbVer, thumbVersion } from "./thumbQueue";

/**
 * A picture from the picture streamer (thumbStream.ts): a canvas the stream draws into — what the disk already has
 * first, then the picture decoded at the box's size. Nothing is requested at mount: the stream decides what to read
 * from where the box is on screen. `bad` shows when the file has no readable picture.
 * The box comes from the parent; `tileW` x `tileH` (css px) when the caller knows it (grids), else it is measured.
 * `fit="contain"` letterboxes instead of cropping (compare views).
 */
export function StreamThumb({ path, mtime, tileW, tileH, bad, fit }: {
  path: string; mtime?: number | null; tileW?: number; tileH?: number; bad: ReactNode; fit?: "contain";
}) {
  const sub = useCallback((f: () => void) => subscribeThumbVer(path, f), [path]);
  const get = useCallback(() => thumbVersion(path), [path]);
  // An fs-delta `modified` bumps the version: the file is read again under a new key.
  const ver = useSyncExternalStore(sub, get);
  const ref = useRef<HTMLCanvasElement>(null);
  const [measured, setMeasured] = useState<{ w: number; h: number } | null>(null);
  const given = tileW !== undefined && tileH !== undefined;
  useLayoutEffect(() => {
    if (given) return;
    const box = ref.current?.parentElement;
    if (!box) return;
    const read = () => {
      const r = box.getBoundingClientRect();
      if (r.width && r.height) setMeasured((m) => (m && m.w === r.width && m.h === r.height ? m : { w: r.width, h: r.height }));
    };
    read();
    const ro = new ResizeObserver(read);
    ro.observe(box);
    return () => ro.disconnect();
  }, [given]);
  const cssW = given ? tileW : measured?.w ?? 0, cssH = given ? tileH : measured?.h ?? 0;
  const dpr = typeof devicePixelRatio === "number" ? Math.max(1, devicePixelRatio) : 1;
  const w = Math.round(cssW * dpr), h = Math.round(cssH * dpr);
  const key = `${path.toLowerCase()}|${mtime ?? 0}|${ver}|${w}x${h}`;
  const [failed, setFailed] = useState<string | null>(null);
  useLayoutEffect(() => {
    const cv = ref.current;
    if (!cv || !w || !h) return;
    cv.width = w;
    cv.height = h;
    cv.dataset.l = "0";
    const ctx = cv.getContext("2d");
    if (!ctx) return;
    ctx.imageSmoothingQuality = "high";
    return thumbStream.register({
      key, path, w, h, el: cv,
      show: (bmp, level) => {
        const [sx, sy, sw, sh, dx, dy, dw, dh] = fit === "contain" ? containRect(bmp.width, bmp.height, w, h) : [...coverRect(bmp.width, bmp.height, w, h), 0, 0, w, h];
        ctx.clearRect(0, 0, w, h);
        ctx.drawImage(bmp, sx, sy, sw, sh, dx, dy, dw, dh);
        cv.dataset.l = String(level);
      },
      fail: () => setFailed(key),
    });
  }, [key, path, w, h, fit]);
  if (failed === key) return <>{bad}</>;
  return <canvas ref={ref} className="th-cv" aria-hidden />;
}
