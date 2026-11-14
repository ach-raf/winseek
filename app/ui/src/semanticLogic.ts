import type { Backend, SemanticHit } from "./types";

export interface SemanticState {
  /** The query these hits were computed for. */
  query: string;
  /** The request (query + scope) these hits answer; see [`semPlan`]. */
  key: string;
  hits: SemanticHit[];
  /** The quick top-k left relevant files out: "Show all" has more than `hits`. */
  more: boolean;
  /** "Show all" is still reading pages. */
  loading: boolean;
}

export const NO_HITS: SemanticState = { query: "", key: "", hits: [], more: false, loading: false };
export const SEM_K = 24;
export const SEM_K_FILTERED = 80; // date / size / extension chips thin the hits out afterwards, so ask for more
/** The full list is read in pages of this size (over 200, which is what tells the backend to list everything). */
export const SEM_PAGE = 1000;

export interface SemPlan {
  key: string;
  /** true: every relevant file, paged; false: the quick top-k. */
  paged: boolean;
  k: number;
}

/**
 * What to ask the backend for. The hits are a pure function of this plan: the same (query, kind scope,
 * ranges) always asks the same thing, whatever was on screen before.
 *
 * A kind scope (a tab or a kind chip) is applied to the hits on the client, so it must not be applied to a
 * top-k: the best 24 hits over everything hold few or no files of the scoped kind. A scoped search therefore
 * reads the whole relevant list (the backend gate decides what is relevant, not a count), like "Show all".
 */
export function semPlan(q: string, scope: string, ranged: boolean, showAll: boolean): SemPlan {
  const paged = showAll || scope !== "all";
  const k = paged ? SEM_PAGE : ranged ? SEM_K_FILTERED : SEM_K;
  return { key: `${q}\u0000${paged ? "all" : k}`, paged, k };
}

/** The hits to show for `plan`: its own answer, or — while it is being computed — the previous query's hits so typing does not flicker. A different scope never shows another scope's hits. */
export function semView(state: SemanticState, q: string, plan: SemPlan): SemanticState {
  if (state.key === plan.key) return state;
  const prevPaged = state.key.endsWith("\u0000all");
  return state.query !== q && prevPaged === plan.paged ? state : { ...NO_HITS, query: "" };
}

/** Runs one plan to completion (first page at once, the rest behind it). `live()` false = superseded: drop everything. */
export async function runSemantic(
  backend: Pick<Backend, "semanticSearch">,
  q: string,
  plan: SemPlan,
  live: () => boolean,
  emit: (s: SemanticState) => void,
): Promise<void> {
  try {
    if (!plan.paged) {
      const r = await backend.semanticSearch(q, plan.k);
      if (live()) emit({ query: q, key: plan.key, hits: r.hits, more: !!r.more, loading: false });
      return;
    }
    let hits: SemanticHit[] = [];
    for (;;) {
      const r = await backend.semanticSearch(q, plan.k, hits.length);
      if (!live() || r.state === "superseded") return;
      hits = hits.concat(r.hits);
      const done = r.hits.length === 0 || hits.length >= (r.total ?? hits.length);
      emit({ query: q, key: plan.key, hits, more: false, loading: !done });
      if (done) return;
    }
  } catch {
    if (live()) emit({ query: q, key: plan.key, hits: [], more: false, loading: false });
  }
}
