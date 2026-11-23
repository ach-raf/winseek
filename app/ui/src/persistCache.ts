import { dehydrate, hydrate, type QueryClient } from "@tanstack/react-query";

/**
 * Stale-while-revalidate across restarts. The in-memory react-query cache already makes tab switches
 * instant; this keeps the Library's heavy lists (pictures, duplicate groups, biggest files,
 * collections and their evaluated hits) in IndexedDB so a fresh window shows the last answer at once
 * and re-reads it in the background. Hydrated entries are invalidated immediately: they render, then
 * are replaced by the fresh answer, so nothing stays stale.
 */

const DB = "winseek-cache";
const STORE = "rq";
const KEY = "library";
/** Bump to drop every persisted answer (shape change). */
const BUSTER = "1";
const MAX_AGE_MS = 7 * 24 * 3600 * 1000;
const WRITE_DELAY_MS = 2000;

const PERSISTED_ROOTS = new Set(["libImages", "libDups", "libBig", "libColls", "collEval"]);
const ROOT_BY_SEG: Record<string, string> = { images: "libImages", dups: "libDups", big: "libBig", colls: "libColls", collEval: "collEval" };
const rootOf = (key: readonly unknown[]): string => (key[0] === "library" ? ROOT_BY_SEG[String(key[1])] ?? "" : "");

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE);
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
async function idbGet<T>(): Promise<T | undefined> {
  const db = await open();
  return new Promise((resolve, reject) => {
    const q = db.transaction(STORE).objectStore(STORE).get(KEY);
    q.onsuccess = () => resolve(q.result as T | undefined);
    q.onerror = () => reject(q.error);
  });
}
async function idbSet(v: unknown): Promise<void> {
  const db = await open();
  return new Promise((resolve, reject) => {
    const t = db.transaction(STORE, "readwrite");
    t.objectStore(STORE).put(v, KEY);
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
  });
}

interface Snapshot { buster: string; at: number; state: ReturnType<typeof dehydrate> }

/** Hydrate the last snapshot (then mark it stale so it is re-read) and keep saving changes. */
export async function restoreAndPersist(qc: QueryClient, version: string): Promise<void> {
  if (typeof indexedDB === "undefined") return;
  const buster = `${BUSTER}:${version}`;
  try {
    const snap = await idbGet<Snapshot>();
    if (snap && snap.buster === buster && Date.now() - snap.at < MAX_AGE_MS) {
      hydrate(qc, snap.state);
      // Shown instantly if a tab reads them, re-read once in the background either way.
      qc.invalidateQueries({ predicate: (q) => PERSISTED_ROOTS.has(rootOf(q.queryKey)) && q.state.dataUpdatedAt > 0 });
    }
  } catch { /* a broken cache is a cold start, never an error */ }

  let timer: number | undefined;
  const save = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      const state = dehydrate(qc, {
        shouldDehydrateQuery: (q) => q.state.status === "success" && PERSISTED_ROOTS.has(rootOf(q.queryKey)),
      });
      idbSet({ buster, at: Date.now(), state } satisfies Snapshot).catch(() => {});
    }, WRITE_DELAY_MS);
  };
  qc.getQueryCache().subscribe((e) => {
    if (e.type === "updated" && e.action.type === "success" && PERSISTED_ROOTS.has(rootOf(e.query.queryKey))) save();
  });
}
