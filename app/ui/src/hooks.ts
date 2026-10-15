import { useCallback, useEffect, useRef, useState } from "react";
import { DEFAULT_GROUPS } from "./modelRows";
import type { Backend, Filter, ModelGroup, ModelsPlan, ModelsProgress, SemanticHit, Tier1Status } from "./types";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

/** Tier 1 (semantic layer) status. Polls every 10 s, or every second while `fast` (setup panel open). */
export function useTier1(backend: Backend, fast: boolean, epoch: number): [Tier1Status | null, () => void] {
  const [st, setSt] = useState<Tier1Status | null>(null);
  const refresh = useCallback(() => {
    backend.tier1Status().then((n) => setSt((p) => (JSON.stringify(p) === JSON.stringify(n) ? p : n))).catch(() => {});
  }, [backend]);
  useEffect(() => {
    refresh();
    const t = window.setInterval(refresh, fast ? 1000 : 10000);
    return () => window.clearInterval(t);
  }, [refresh, fast, epoch]);
  return [st, refresh];
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

export interface SemanticState {
  /** The query these hits were computed for. */
  query: string;
  hits: SemanticHit[];
}

const DEBOUNCE_MS = 150;
const SEM_K = 24;
const SEM_K_FILTERED = 80; // chips thin the hits out afterwards, so ask for more

/**
 * Semantic hits for the query, ~150 ms after typing stops. The previous hits stay on screen
 * until the new ones arrive (no flicker / reshuffle); disabling or clearing the query drops them.
 * `reload` re-runs the search for the same query (popup re-shown, file deleted).
 */
export function useSemantic(backend: Backend, query: string, enabled: boolean, filter: Filter, reload: number, filtered = false): SemanticState {
  const [state, setState] = useState<SemanticState>({ query: "", hits: [] });
  const seq = useRef(0);
  const q = query.trim();
  const active = enabled && q.length >= 2 && (filter === "all" || filter === "images" || filter === "docs");
  useEffect(() => {
    const my = ++seq.current;
    if (!active) {
      setState((s) => (s.hits.length ? { query: "", hits: [] } : s));
      return;
    }
    const t = window.setTimeout(() => {
      backend.semanticSearch(q, filtered ? SEM_K_FILTERED : SEM_K).then((r) => {
        if (my === seq.current) setState({ query: q, hits: r.hits });
      }).catch(() => { if (my === seq.current) setState({ query: q, hits: [] }); });
    }, DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [backend, q, active, reload, filtered]);
  return state;
}
