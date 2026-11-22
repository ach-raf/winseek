import { useCallback, useEffect, useRef, useState } from "react";
import { errText } from "./format";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DEFAULT_GROUPS } from "./modelRows";
import { NO_HITS, runSemantic, semPlan, semPlaceholder, type SemanticState } from "./semanticLogic";
import { qk } from "./qk";
import { parseSection, type SettingsSection } from "./settingsNav";
import type { Backend, Filter, ModelGroup, ModelsPlan, ModelsProgress, Tier1Status } from "./types";


/** Tier 1 (semantic layer) status, owned by react-query: one shared cache entry for every consumer
 *  (the popup and the Library used to poll independently, two fetches for one answer). Polls every
 *  10 s, or every second while `fast` (setup panel open). */
export function useTier1(backend: Backend, fast: boolean): [Tier1Status | null, () => void] {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: qk.t1, queryFn: () => backend.tier1Status(), refetchInterval: fast ? 1000 : 10000 });
  const refresh = useCallback(() => { void qc.invalidateQueries({ queryKey: qk.t1 }); }, [qc]);
  return [q.data ?? null, refresh];
}

export interface Models {
  plan: ModelsPlan | null;
  running: boolean;
  progress: ModelsProgress | null;
  /** "cancelled" after Cancel (partial files kept), else the failure text, else null. */
  interrupted: string | null;
  /** Components of the running (or last) download; null when the app started it on its own (first run). */
  groups: ModelGroup[] | null;
  /** Bytes received per plan item in this download (Settings rows add these up). */
  got: Record<string, number>;
  /** `includeParser`: also download the optional document parser; `groups`: only these components. */
  start(includeParser?: boolean, groups?: ModelGroup[]): void;
  cancel(): void;
  refreshPlan(): void;
}

/** Model download state, fed by the `models-progress` / `models-done` events. Lives in App so it survives the panel closing. */
export function useModels(backend: Backend, onDone: () => void): Models {
  const [plan, setPlan] = useState<ModelsPlan | null>(null);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<ModelsProgress | null>(null);
  const [interrupted, setInterrupted] = useState<string | null>(null);
  const [groups, setGroups] = useState<ModelGroup[] | null>(null);
  const [got, setGot] = useState<Record<string, number>>({});
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;

  const refreshPlan = useCallback(() => {
    backend.modelsPlan().then((p) => { setPlan(p); if (p.downloading) setRunning(true); }).catch(() => {});
  }, [backend]);

  useEffect(() => {
    const offs: (() => void)[] = [];
    let dead = false;
    const keep = (p: Promise<() => void>) => p.then((u) => (dead ? u() : offs.push(u)));
    keep(backend.onModelsProgress((p) => {
      setRunning(true);
      setProgress(p);
      setGot((g) => (g[p.item] === p.downloaded ? g : { ...g, [p.item]: p.downloaded }));
    }));
    keep(backend.onModelsDone((d) => {
      setRunning(false);
      if (d.ok) {
        setProgress(null);
        setInterrupted(null);
        refreshPlan();
        onDoneRef.current();
      } else {
        setInterrupted(d.error ?? "Download failed");
        refreshPlan(); // parts that did finish show as installed
      }
    }));
    return () => { dead = true; offs.forEach((u) => u()); };
  }, [backend, refreshPlan]);

  const start = useCallback((includeParser?: boolean, only?: ModelGroup[]) => {
    setInterrupted(null);
    setRunning(true);
    setProgress(null);
    setGot({});
    setGroups(only ?? [...DEFAULT_GROUPS, ...(includeParser ? ["parser" as const] : [])]);
    backend.modelsDownload(includeParser, only).catch((e) => { setRunning(false); setInterrupted(errText(e)); });
  }, [backend]);
  const cancel = useCallback(() => { backend.modelsCancel().catch(() => {}); }, [backend]);

  return { plan, running, progress, interrupted, groups, got, start, cancel, refreshPlan };
}

const DEBOUNCE_MS = 150;
export type { SemanticState } from "./semanticLogic";

/** The text a meaning search actually fetches for: typing waits out the debounce; a changed scope
 *  (same text, different plan / reload) fetches at once — the user already decided, and waiting out
 *  a typing pause that never comes makes every tab switch feel laggy. Deactivated (popup empty or
 *  a name-only scope) resets to "" so the next activation starts fresh. */
function useDebouncedScope(active: boolean, q: string, planKey: string, reload: number): string {
  const [eff, setEff] = useState(active ? q : "");
  const lastQ = useRef(q);
  useEffect(() => {
    if (!active) { setEff(""); return; }
    const sameQ = lastQ.current === q;
    lastQ.current = q;
    if (sameQ) { setEff(q); return; }
    const t = window.setTimeout(() => setEff(q), DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [q, active, planKey, reload]);
  return eff;
}

/**
 * Semantic hits for the query, owned by react-query: every (epoch, text, plan) is a cache entry, so
 * an in-flight fetch can never land in another scope's view and answers survive scope round-trips.
 * While a new query is computed the previous hits stay on screen (no flicker, `semPlaceholder`);
 * a changed scope (tab / chip) never shows another scope's hits. The result is a function of
 * (query, scope, ranges, showAll) only, see `semPlan`.
 * `reload` re-runs the search for the same query (popup re-shown, file deleted) by pinning a fresh
 * epoch into the key.
 */
export function useSemantic(backend: Backend, query: string, enabled: boolean, filter: Filter, reload: number, filtered = false, all = false): SemanticState {
  const qc = useQueryClient();
  const q = query.trim();
  // Every kind the meaning index holds; folders, apps and settings are found by name only.
  const active = enabled && q.length >= 2 && (filter === "all" || filter === "images" || filter === "docs" || filter === "video" || filter === "audio");
  const plan = semPlan(q, filter, filtered, all);
  const eff = useDebouncedScope(active, q, plan.key, reload);
  // The fetch runs for the effective text; the plan must come from the same text, or the debounce
  // window would key old text together with the new scope's shape.
  const effPlan = semPlan(eff, filter, filtered, all);
  const semQuery = useQuery({
    queryKey: qk.sem(reload, eff, effPlan.key),
    queryFn: (ctx) => {
      // The paged reads stream into the cache as they land (runSemantic emits partial states), so
      // "Show all" fills progressively instead of holding an empty list until the last page.
      let last = NO_HITS;
      return runSemantic(backend, eff, effPlan, () => !ctx.signal.aborted, (s) => {
        last = s;
        qc.setQueryData(ctx.queryKey, s);
      }).then(() => last);
    },
    enabled: active && eff.length >= 2,
    placeholderData: (prev, prevQuery) => semPlaceholder(prev, prevQuery?.queryKey[2] as string | undefined, eff, effPlan),
    staleTime: 0, // a cached answer is shown instantly, then re-checked in the background
    gcTime: 60_000,
  });
  return active ? semQuery.data ?? NO_HITS : NO_HITS;
}

/** A small preference mirrored into localStorage (best-effort: with storage off, the choice just is
 *  not remembered). `read` parses the stored string; anything it rejects becomes the fallback inside it. */
export function useStoredValue<T>(key: string, read: (raw: string | null) => T, write: (v: T) => string): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try { return read(localStorage.getItem(key)); } catch { return read(null); }
  });
  const put = useCallback((n: T) => {
    setV(n);
    try { localStorage.setItem(key, write(n)); } catch { /* storage unavailable: not remembered */ }
  }, [key, write]);
  return [v, put];
}

/** The last Settings section, remembered across sessions — one reader for the popup and the
 *  Library window, which share the storage key (they used to be two try/catch copies). */
export function useStoredSection(): [SettingsSection, (s: SettingsSection) => void] {
  return useStoredValue("winseek.settingsSection", parseSection, (s) => s);
}

/** Session-scoped state: survives unmount/remount (a Settings section switch, closing and
 *  reopening the page) but is dropped when the app exits, unlike `useStoredValue`. For display
 *  state too bulky to greet every launch expanded, but too sticky to lose on the way back. */
const sessionSlots = new Map<string, unknown>();
export function useSessionState<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => (sessionSlots.has(key) ? (sessionSlots.get(key) as T) : initial));
  const put = useCallback((n: T) => { sessionSlots.set(key, n); setV(n); }, [key]);
  return [v, put];
}

/** `value`, but a change reaches the caller only after it has stayed put for `ms` (trailing
 *  debounce: the fetch or save fires once, when the typing / slider goes quiet). */
export function useDebouncedValue<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  const ref = useRef(value);
  ref.current = value;
  useEffect(() => {
    if (Object.is(v, value)) return;
    const t = window.setTimeout(() => setV(ref.current), ms);
    return () => window.clearTimeout(t);
  }, [value, ms, v]);
  return v;
}

/** `value`, but a change shows up at most once per `ms` (the last one always does). For inputs
 *  that climb steadily (the settled embedding count): a trailing debounce would never fire while
 *  the climb goes on, this re-reads at a bounded rate and catches up when it ends. */
export function useSettled<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  const last = useRef(0);
  useEffect(() => {
    if (Object.is(v, value)) return;
    const t = window.setTimeout(() => { last.current = Date.now(); setV(value); }, Math.max(0, last.current + ms - Date.now()));
    return () => window.clearTimeout(t);
  }, [value, ms, v]);
  return v;
}

/** What a preview pane shows, from `make()`, refreshed only after the selection has settled for
 *  `ms` (arrowing through a list must not load a preview per row) — but a `null` key (nothing to
 *  show) and the first pick after an empty pane apply at once: a click must open instantly.
 *  Keyed by `key`, because a fresh object every render would reset the timer forever. App's
 *  popup pane and Gallery's lead preview are the same logic; they used to be two diverged copies. */
export function usePaneFollow<T>(key: string | null, ms: number, make: () => T | null): T | null {
  const [v, setV] = useState<T | null>(null);
  const shownKey = useRef<string | null>(null); // the key `v` was built from
  const makeRef = useRef(make);
  makeRef.current = make;
  useEffect(() => {
    const apply = () => { shownKey.current = key; setV(key === null ? null : makeRef.current()); }; // no key = nothing to follow: the pane empties (never build a value from a selection that is not there)
    if (key === null || shownKey.current === null) apply();
    else if (shownKey.current !== key) {
      const t = window.setTimeout(apply, ms);
      return () => window.clearTimeout(t);
    }
  }, [key, ms]);
  return v;
}
