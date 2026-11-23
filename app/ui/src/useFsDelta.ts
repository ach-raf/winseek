import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { fsDeltaBus } from "./fsDeltaPatch";
import { bumpThumbs } from "./thumbQueue";
import { CountWatchdog } from "./fsWatchdog";
import { qk } from "./qk";
import type { Backend, FsDelta, VolumeStatus } from "./types";

/** Pictures follow a delta: changed (or re-created) files are read again under a new version. Gone files need
 *  nothing: their tiles leave, and the stream's pool lets their pictures go with the least recently shown. */
function thumbsFollow(d: FsDelta) {
  bumpThumbs([...d.modified, ...d.added, ...d.renamed.map((r) => r.to)]);
}

/**
 * The window's one `fs-delta` subscription (mount it ONCE per window: the popup's App, the standalone Library):
 * every batch the backend pushes is patched into this window's query cache (see `fsDeltaPatch`) and handed to
 * the window's listeners (`useFsDeltaListener`).
 */
export function useFsDelta(backend: Backend, enabled = true): void {
  const qc = useQueryClient();
  useEffect(() => {
    if (!enabled) return;
    const bus = fsDeltaBus(qc);
    const offThumbs = bus.on(thumbsFollow);
    // Safety net: counts moved but no delta came -> the stream missed something; re-read the old way.
    const dog = new CountWatchdog();
    const offDog = bus.on(() => dog.onDelta(Date.now()));
    const offStatus = qc.getQueryCache().subscribe((ev) => {
      if (ev.type !== "updated" || ev.query.queryKey[0] !== qk.libStatus[0] || ev.query.queryKey[1] !== qk.libStatus[1]) return;
      const vols = ev.query.state.data as VolumeStatus[] | undefined;
      if (vols && dog.onStatus(vols, Date.now())) bus.push({ removed: [], renamed: [], added: [], modified: [], dirs: [], overflow: true });
    });
    let off: (() => void) | undefined;
    let dead = false;
    backend.onFsDelta((d) => bus.push(d)).then((u) => { if (dead) u(); else off = u; }).catch(console.error);
    return () => { dead = true; off?.(); offThumbs(); offDog(); offStatus(); };
  }, [backend, qc, enabled]);
}

/** Run `cb` for every delta this window sees (the backend's and the app's own); always the latest `cb`. */
export function useFsDeltaListener(cb: (d: FsDelta) => void): void {
  const qc = useQueryClient();
  const ref = useRef(cb);
  useEffect(() => { ref.current = cb; });
  useEffect(() => fsDeltaBus(qc).on((d) => ref.current(d)), [qc]);
}
