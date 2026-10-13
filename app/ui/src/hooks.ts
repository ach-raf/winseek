import { useCallback, useEffect, useRef, useState } from "react";
import type { Backend, Filter, ModelsPlan, ModelsProgress, SemanticHit, Tier1Status } from "./types";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

/** Tier 1 (semantic layer) status. Polls every 10 s, or every second while `fast` (setup panel open). */
export function useTier1(backend: Backend, fast: boolean, epoch: number): [Tier1Status | null, () => void] {
  const [st, setSt] = useState<Tier1Status | null>(null);
  const refresh = useCallback(() => {
    backend.tier1Status().then(setSt).catch(() => {});
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
  start(): void;
  cancel(): void;
  refreshPlan(): void;
}

/** Model download state, fed by the `models-progress` / `models-done` events. Lives in App so it survives the panel closing. */
export function useModels(backend: Backend, onDone: () => void): Models {
  const [plan, setPlan] = useState<ModelsPlan | null>(null);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<ModelsProgress | null>(null);
  const [interrupted, setInterrupted] = useState<string | null>(null);
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;

  const refreshPlan = useCallback(() => {
    backend.modelsPlan().then((p) => { setPlan(p); if (p.downloading) setRunning(true); }).catch(() => {});
  }, [backend]);

  useEffect(() => {
    const offs: (() => void)[] = [];
    let dead = false;
    const keep = (p: Promise<() => void>) => p.then((u) => (dead ? u() : offs.push(u)));
    keep(backend.onModelsProgress((p) => { setRunning(true); setProgress(p); }));
    keep(backend.onModelsDone((d) => {
      setRunning(false);
      if (d.ok) {
        setProgress(null);
        setInterrupted(null);
        refreshPlan();
        onDoneRef.current();
      } else {
        setInterrupted(d.error ?? "Download failed");
      }
    }));
    return () => { dead = true; offs.forEach((u) => u()); };
  }, [backend, refreshPlan]);

  const start = useCallback(() => {
    setInterrupted(null);
    setRunning(true);
    backend.modelsDownload().catch((e) => { setRunning(false); setInterrupted(errText(e)); });
  }, [backend]);
  const cancel = useCallback(() => { backend.modelsCancel().catch(() => {}); }, [backend]);

  return { plan, running, progress, interrupted, start, cancel, refreshPlan };
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
  const active = enabled && q.length >= 2 && (filter === "all" || filter === "images");
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
