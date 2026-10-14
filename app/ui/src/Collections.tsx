import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, Collection, LibImage, SemanticHit, Tier1Status } from "./types";
import { Gallery, type GallerySection, type GridItem, type LibActions } from "./Gallery";
import { baseName } from "./Semantic";
import { isScreenshot } from "./libraryLogic";

interface Eval {
  status: "loading" | "ready" | "off" | "error";
  hits: SemanticHit[];
  error?: string;
  /** What the hits were computed for. */
  sig: string;
}

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const CAP = 500;
/** EmbeddingGemma-2 image/query cosines: photos reach ~0.62, collection members sit ~0.61-0.75
 *  (measured with `winseek-eval route`), so the useful slider range is narrow and high. */
const THRESHOLD_MIN = 0.4;
const THRESHOLD_MAX = 0.9;
const NEW_THRESHOLD = 0.62;
const sigOf = (c: Collection, gen: number) => `${c.query}|${c.threshold}|${gen}`;

interface Draft {
  /** Name of the collection being edited; null = new. */
  orig: string | null;
  name: string;
  query: string;
  threshold: number;
}

export function Collections({ backend, images, hidden, t1, gen, epoch, actions, onError }: {
  backend: Backend;
  /** Visible timeline images (for the name-rule collections). */
  images: LibImage[];
  hidden: ReadonlySet<string>;
  t1: Tier1Status | null;
  /** Bumps when new embeddings arrived (collections are evaluated again). */
  gen: number;
  epoch: number;
  actions: LibActions;
  onError: (e: unknown) => void;
}) {
  const [list, setList] = useState<Collection[] | null>(null);
  const [evals, setEvals] = useState<Record<string, Eval>>({});
  const [selName, setSelName] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [confirmDel, setConfirmDel] = useState(false);
  const [formErr, setFormErr] = useState<string | null>(null);
  const run = useRef(0);
  const sigs = useRef<Record<string, string>>({});

  useEffect(() => { backend.libraryCollections().then(setList).catch(onError); }, [backend, onError]);

  // (Re)evaluate every query collection: on open, when it changed, when new embeddings arrived, after an undo.
  const evalAll = useCallback(async (cs: Collection[]) => {
    const my = ++run.current;
    for (const c of cs) {
      if (c.rule) continue;
      const sig = sigOf(c, gen);
      if (sigs.current[c.name] === sig) continue;
      setEvals((m) => ({ ...m, [c.name]: { status: "loading", hits: m[c.name]?.hits ?? [], sig } }));
      try {
        const r = await backend.libraryCollection(c.query, c.threshold, CAP);
        if (run.current !== my) return;
        const off = r.state === "models_missing" || r.state === "unavailable";
        if (!off) sigs.current[c.name] = sig; // "off" is re-asked next time (the models may arrive)
        setEvals((m) => ({ ...m, [c.name]: { status: off ? "off" : "ready", hits: r.hits, sig } }));
      } catch (e) {
        if (run.current !== my) return;
        setEvals((m) => ({ ...m, [c.name]: { status: "error", hits: [], error: errText(e), sig } }));
      }
    }
  }, [backend, gen]);

  useEffect(() => { sigs.current = {}; }, [epoch]);
  useEffect(() => { if (list) void evalAll(list); }, [list, evalAll, epoch]);

  useEffect(() => {
    if (!list?.length) return;
    if (!selName || !list.some((c) => c.name === selName)) setSelName(list[0].name);
  }, [list, selName]);

  const sel = list?.find((c) => c.name === selName) ?? null;

  const shots = useMemo(
    () => images.filter((i) => isScreenshot(baseName(i.path))),
    [images],
  );
  const hitsOf = useCallback((c: Collection): GridItem[] => {
    if (c.rule === "screenshots") return shots.map((i) => ({ path: i.path, name: baseName(i.path), size: i.size, mtime: i.mtime }));
    return (evals[c.name]?.hits ?? [])
      .filter((h) => !hidden.has(h.path))
      .map((h) => ({ path: h.path, name: baseName(h.path), size: h.size ?? null, mtime: h.mtime ?? null, score: h.score }));
  }, [evals, hidden, shots]);

  const save = useCallback(async (next: Collection[]): Promise<boolean> => {
    try {
      setList(await backend.librarySetCollections(next));
      setFormErr(null);
      return true;
    } catch (e) {
      setFormErr(errText(e));
      return false;
    }
  }, [backend]);

  // The threshold slider commits shortly after it stops moving.
  const [thr, setThr] = useState<number | null>(null);
  useEffect(() => { setThr(null); setConfirmDel(false); }, [selName]);
  useEffect(() => {
    if (thr === null || !sel || !list || thr === sel.threshold) return;
    const t = window.setTimeout(() => { void save(list.map((c) => (c.name === sel.name ? { ...c, threshold: thr } : c))); }, 450);
    return () => window.clearTimeout(t);
  }, [thr, sel, list, save]);

  const items = useMemo(() => (sel ? hitsOf(sel) : []), [sel, hitsOf]);
  const sections: GallerySection[] = useMemo(() => [{ key: sel?.name ?? "", label: null, items }], [items, sel?.name]);
  const ev = sel && !sel.rule ? evals[sel.name] : undefined;

  const emptyMsg = !sel ? null
    : sel.rule ? <><strong>No screenshots yet</strong><span>Files named like “Screenshot (12).png” appear here.</span></>
    : ev?.status === "loading" || !ev ? <><span className="spin" aria-label="Loading" /><span>Searching…</span></>
    : ev.status === "off" ? <><strong>Search by meaning is not set up</strong><span>Download the models in the popup (Ctrl+,) and this collection fills itself.</span></>
    : ev.status === "error" ? <><strong>Could not search</strong><span>{ev.error}</span></>
    : <><strong>Nothing matches yet</strong><span>Lower the threshold, or wait for indexing to reach more photos.</span></>;

  const count = (c: Collection) => {
    if (c.rule === "screenshots") return shots.length;
    const e = evals[c.name];
    return e && e.status === "ready" ? e.hits.filter((h) => !hidden.has(h.path)).length : null;
  };

  const submit = async () => {
    if (!draft || !list) return;
    const c: Collection = { name: draft.name, query: draft.query, threshold: draft.threshold };
    const old = draft.orig ? list.find((x) => x.name === draft.orig) : undefined;
    if (old?.rule) c.rule = old.rule;
    const next = draft.orig ? list.map((x) => (x.name === draft.orig ? c : x)) : [...list, c];
    if (await save(next)) { setSelName(c.name.trim()); setDraft(null); }
  };

  const del = async () => {
    if (!sel || !list) return;
    if (await save(list.filter((c) => c.name !== sel.name))) { setSelName(null); setConfirmDel(false); }
  };

  const embedded = t1?.stats?.done;
  return (
    <div className="co">
      <aside className="co-side">
        <div className="co-list" role="listbox" aria-label="Smart collections">
          {list?.map((c) => {
            const n = count(c);
            return (
              <button key={c.name} role="option" aria-selected={c.name === selName} className={"co-item" + (c.name === selName ? " on" : "")} onClick={() => { setSelName(c.name); setDraft(null); }}>
                <span className="co-name">{c.name}</span>
                <span className="co-n">{n === null ? (evals[c.name]?.status === "loading" ? "…" : "–") : n.toLocaleString("en-US")}</span>
              </button>
            );
          })}
        </div>
        <button className="btn quiet co-new" onClick={() => { setDraft({ orig: null, name: "", query: "", threshold: NEW_THRESHOLD }); setFormErr(null); }}>+ New collection</button>
        <p className="co-note">Each collection is a saved search by meaning. {embedded !== undefined && `${embedded.toLocaleString("en-US")} photos understood so far; collections refresh as indexing goes on.`}</p>
      </aside>

      <section className="co-main">
        {draft ? (
          <form className="co-form" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
            <h2>{draft.orig ? "Edit collection" : "New collection"}</h2>
            <label>Name<input autoFocus value={draft.name} maxLength={60} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="Receipts" /></label>
            {!(draft.orig && list?.find((c) => c.name === draft.orig)?.rule) && (
              <>
                <label>Describe what to find<input value={draft.query} onChange={(e) => setDraft({ ...draft, query: e.target.value })} placeholder="a photo of a paper receipt" /></label>
                <label>Minimum match: <b>{draft.threshold.toFixed(2)}</b>
                  <input type="range" min={THRESHOLD_MIN} max={THRESHOLD_MAX} step={0.01} value={draft.threshold} onChange={(e) => setDraft({ ...draft, threshold: Number(e.target.value) })} />
                </label>
              </>
            )}
            {formErr && <div className="co-err" role="alert">{formErr}</div>}
            <div className="co-btns">
              <button type="submit" className="btn primary">Save</button>
              <button type="button" className="btn quiet" onClick={() => { setDraft(null); setFormErr(null); }}>Cancel</button>
            </div>
          </form>
        ) : sel ? (
          <>
            <header className="co-head">
              <div className="co-title">
                <h2>{sel.name}</h2>
                <p>{sel.rule === "screenshots" ? "Files whose name looks like a screenshot." : `“${sel.query}”`}</p>
              </div>
              {!sel.rule && (
                <label className="co-thr" title="Minimum cosine match for a photo to belong to the collection">
                  Match ≥ <b>{(thr ?? sel.threshold).toFixed(2)}</b>
                  <input type="range" min={THRESHOLD_MIN} max={THRESHOLD_MAX} step={0.01} value={thr ?? sel.threshold} onChange={(e) => setThr(Number(e.target.value))} />
                </label>
              )}
              <div className="co-btns">
                <button className="btn quiet" onClick={() => { setDraft({ orig: sel.name, name: sel.name, query: sel.query, threshold: sel.threshold }); setFormErr(null); }}>Edit</button>
                {confirmDel ? (
                  <>
                    <button className="btn primary" onClick={() => void del()}>Delete collection</button>
                    <button className="btn quiet" onClick={() => setConfirmDel(false)}>Keep</button>
                  </>
                ) : (
                  <button className="btn quiet" onClick={() => setConfirmDel(true)}>Delete</button>
                )}
              </div>
            </header>
            {formErr && <div className="co-err" role="alert">{formErr}</div>}
            <div className="co-grid">
              <Gallery backend={backend} sections={sections} actions={actions} empty={emptyMsg} />
            </div>
          </>
        ) : (
          <div className="lg-empty"><strong>No collections</strong><span>Create one with “New collection”.</span></div>
        )}
      </section>
    </div>
  );
}
