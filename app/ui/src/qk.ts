import { QueryClient } from "@tanstack/react-query";

/**
 * The react-query layer: client defaults, the query-key factory and the invalidation entry points,
 * in one place. Everything that reads a query and everything that invalidates it uses the same
 * factory, so a key can never drift between the two (single source of truth per data type).
 */

/** One client per window: the popup and the library window load the bundle separately, so each
 *  JS context gets its own cache. Defaults mirror how winseek's local IPC behaves: answers come
 *  from our own process and only change when the app itself says so — no retries, no refetch on
 *  focus/reconnect; staleness is decided by explicit invalidation, never by time. */
export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        staleTime: Infinity,
      },
    },
  });
}

/** The query keys. */
export const qk = {
  /** Semantic-layer status, polled (1 s with a setup panel open, 10 s otherwise). One shared entry
   *  for every consumer — the popup and the Library used to poll independently. */
  t1: ["t1"] as const,
  /** The Library's whole picture list: only Collections and Clean up read it (opened on demand). */
  libImages: ["library", "images"] as const,
  /** The timeline's base list inside the backend (a load id; re-read when the index changes). */
  libTimelineBase: ["library", "timeline", "base"] as const,
  /** The timeline's filtered view: months and counts of one load, view tab, name filter and hidden set. Never cached
   *  (`gcTime: 0`): the backend answers rows only for its newest view, so an old answer must not come back. */
  libTimeline: (load: number, view: string, q: string, hiddenKey: string) => ["library", "timeline", "view", load, view, q, hiddenKey] as const,
  /** Exact duplicate groups. */
  libDups: ["library", "dups"] as const,
  /** The largest files. */
  libBig: ["library", "big"] as const,
  /** The saved collection definitions (only ever written by `librarySetCollections`). */
  libColls: ["library", "colls"] as const,
  /** One query collection's evaluated hits; `sig` pins query/threshold/exclusions/generation/state. */
  collEval: (name: string, sig: string) => ["library", "collEval", name, sig] as const,
  /** Image dimensions for one duplicate group (keyed by the member paths). */
  dims: (pathsKey: string) => ["library", "dims", pathsKey] as const,
  /** The standalone library window's volume poll. */
  libStatus: ["library", "status"] as const,
  /** The Windows service's install/run state (read while the popup shows demo data). */
  service: ["service"] as const,
  /** The whole status page: the Settings›Status panes read it on a 1 s clock and the bottom jobs
   *  bar rides the same entry, so an open Settings never means a second poller in one window. */
  statusPage: ["statusPage"] as const,
  /** One Space screen folder roll-up; `p` is the folder path, "" = the drive list. */
  spaceDir: (p: string) => ["space", "dir", p] as const,
  /** One Space folder's type / biggest-files detail. */
  spaceDetail: (p: string) => ["space", "detail", p] as const,
  /** One meaning-search answer (popup), pinned to the popup epoch (re-show), the typed text and the plan. */
  sem: (epoch: number, q: string, planKey: string) => ["sem", epoch, q, planKey] as const,
} as const;

/** The index lists that can change while the Library is hidden (files deleted in the search
 *  popup): re-read when the Library becomes the visible page again (the old `reveal` bump).
 *  Collection evaluations are deliberately not here — they re-run only when their inputs change. */
export function refreshLibraryIndex(qc: QueryClient): void {
  qc.invalidateQueries({ queryKey: qk.libImages });
  qc.invalidateQueries({ queryKey: qk.libTimelineBase });
  qc.invalidateQueries({ queryKey: qk.libDups });
  qc.invalidateQueries({ queryKey: qk.libBig });
}

/** New embeddings arrived (the settled `done` count moved): the picture list and dup groups are
 *  read again; collections re-key themselves because the generation is part of their sig. */
export function refreshLibraryEmbeddings(qc: QueryClient): void {
  qc.invalidateQueries({ queryKey: qk.libImages });
  qc.invalidateQueries({ queryKey: qk.libTimelineBase });
  qc.invalidateQueries({ queryKey: qk.libDups });
}

/** An undo put files back: the index lists and every collection's evaluated hits are read again
 *  (the collection definitions themselves are only ever written from this window's saves). */
export function refreshLibraryAfterUndo(qc: QueryClient): void {
  refreshLibraryIndex(qc);
  qc.invalidateQueries({ queryKey: ["library", "collEval"] });
}

/** The Space screen's roll-ups may be behind the index (a recycle, an undo, a new scan, or the
 *  Refresh button): every folder slice and detail is read again; cached answers keep rendering. */
export function refreshSpace(qc: QueryClient): void {
  qc.invalidateQueries({ queryKey: ["space"] });
}
