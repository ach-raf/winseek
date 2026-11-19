import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { lowThumbs, requestThumb, subscribeThumbVer, thumbs, thumbVersion, withThumbVer } from "./thumbQueue";

const noSub = () => () => {};
const zero = () => 0;

/** The re-read count of `path` (an fs-delta `modified` bumps it). Subscribed by path: a bump re-renders only the
 *  tiles that show that file. */
function useThumbVer(path: string | undefined): number {
  const sub = useCallback((f: () => void) => subscribeThumbVer(path!, f), [path]);
  const get = useCallback(() => thumbVersion(path), [path]);
  return useSyncExternalStore(path ? sub : noSub, path ? get : zero);
}

type Tier = "wait" | "ok" | "bad";
interface St { url: string; sharp: Tier; low: boolean }

/**
 * A thumbnail that asks the page's queues for its picture instead of firing the request at mount: tiles scrolled
 * past before their turn never cost the backend anything. With `low` (the 32 px tier) the tile shows that first,
 * blurred, from the fast lane (never held by a fling), then cross-fades to the sharp picture once the sharp queue
 * gets to it; nothing changes size. Without `low` a flat block waits. A failed sharp load shows `bad`. With `path`,
 * a change of that file on disk (fs-delta `modified`) re-requests both tiers under a new url.
 * The parent sets the box (width/height or aspect-ratio); this fills it (`position: absolute; inset: 0`).
 */
export function ThumbImg({ src: base, low: lowBase, bad, path, fit }: {
  src: string; low?: string | null; bad: React.ReactNode; path?: string;
  /** `contain` letterboxes instead of cropping (compare views). */
  fit?: "contain";
}) {
  const r = useThumbVer(path);
  const src = withThumbVer(base, r);
  const low = lowBase && lowBase !== base ? withThumbVer(lowBase, r) : null;
  const [st, setSt] = useState<St>(() => ({ url: src, sharp: thumbs.has(src) ? "ok" : "wait", low: !!low && lowThumbs.has(low) }));
  // A new url (another file in a recycled tile, a re-read) starts over — derived during render, not in an effect,
  // so the old picture never flashes under the new name.
  let cur = st;
  if (st.url !== src) {
    cur = { url: src, sharp: thumbs.has(src) ? "ok" : "wait", low: !!low && lowThumbs.has(low) };
    setSt(cur);
  }
  useEffect(() => {
    if (thumbs.has(src)) return;
    return requestThumb(src, low,
      () => setSt((s) => (s.url === src && !s.low ? { ...s, low: true } : s)),
      (ok) => setSt((s) => (s.url === src ? { ...s, sharp: ok ? "ok" : "bad" } : s)));
  }, [src, low]);
  if (cur.sharp === "bad") return <>{bad}</>;
  return (
    <span className={"th" + (fit === "contain" ? " th-c" : "")} aria-hidden>
      {cur.low && low ? <img src={low} alt="" draggable={false} decoding="async" className="th-lo" />
        : cur.sharp !== "ok" && <span className="th-wait" />}
      {cur.sharp === "ok" && <img src={src} alt="" draggable={false} decoding="async" className="th-in" />}
    </span>
  );
}
