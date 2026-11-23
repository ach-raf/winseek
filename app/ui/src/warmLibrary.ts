import type { QueryClient } from "@tanstack/react-query";
import type { Backend } from "./types";
import { qk, BIG_FILES_N } from "./qk";

/**
 * Warm the Library's lists while the user is busy elsewhere (typing a search, just launched the
 * app), so Clean up / duplicates / Collections open on data that is already there. One list at a
 * time, each after an idle gap, so a search the user starts meanwhile is never starved. Entries that
 * are already fresh are skipped (staleTime is Infinity), restored-but-invalidated ones are re-read.
 * Failures are ignored: the screen that needs the list will read it itself.
 */
const START_DELAY_MS = 2500;
const GAP_MS = 600;

const idle = (ms: number) => new Promise<void>((resolve) => {
  window.setTimeout(() => {
    const ric = (window as unknown as { requestIdleCallback?: (cb: () => void, o?: { timeout: number }) => void }).requestIdleCallback;
    if (ric) ric(() => resolve(), { timeout: 3000 }); else resolve();
  }, ms);
});

export function warmLibrary(qc: QueryClient, backend: Backend): void {
  const steps: Array<() => Promise<unknown>> = [
    () => qc.prefetchQuery({ queryKey: qk.libColls, queryFn: () => backend.libraryCollections() }),
    () => qc.prefetchQuery({ queryKey: qk.libDups, queryFn: () => backend.libraryDupGroups() }),
    () => qc.prefetchQuery({ queryKey: qk.libBig, queryFn: () => backend.libraryBigFiles(BIG_FILES_N) }),
    () => qc.prefetchQuery({ queryKey: qk.libImages, queryFn: () => backend.libraryImages() }),
  ];
  void (async () => {
    await idle(START_DELAY_MS);
    for (const step of steps) {
      try { await step(); } catch { /* the screen reads it itself */ }
      await idle(GAP_MS);
    }
  })();
}
