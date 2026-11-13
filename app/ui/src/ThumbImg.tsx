import { useEffect, useState } from "react";
import { thumbs } from "./thumbQueue";

/**
 * A thumbnail that asks the page's `ThumbQueue` for its picture instead of firing the request at mount: tiles
 * scrolled past before their turn never cost the backend anything. Until it loads the tile shows `fallback`
 * (a soft shimmer by default); a failed load shows `bad`.
 */
export function ThumbImg({ src, bad }: { src: string; bad: React.ReactNode }) {
  const [state, setState] = useState<"wait" | "ok" | "bad">(() => (thumbs.has(src) ? "ok" : "wait"));
  useEffect(() => {
    if (thumbs.has(src)) { setState("ok"); return; }
    setState("wait");
    return thumbs.request(src, (ok) => setState(ok ? "ok" : "bad"));
  }, [src]);
  if (state === "bad") return <>{bad}</>;
  if (state === "wait") return <span className="th-wait" aria-hidden />;
  return <img src={src} alt="" draggable={false} decoding="async" className="th-in" />;
}
