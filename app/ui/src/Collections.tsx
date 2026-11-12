import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, Collection, LibImage, SemanticHit, Tier1Status } from "./types";
import { Gallery, type GallerySection, type GridExtra, type GridItem, type LibActions } from "./Gallery";
import { baseName } from "./Semantic";
import { kindOf } from "./format";
import { freshIdeas } from "./libraryLogic";
import { IChevL, IPlus } from "./icons";

interface Eval {
  status: "loading" | "ready" | "off" | "error";
  hits: SemanticHit[];
  error?: string;
  /** What the hits were computed for. */
  sig: string;
}

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const CAP = 500;
/** The slider is a strictness dial over the calibrated gate (hub-corrected, per-modality z-score —
 *  the same relevance machinery search uses), not a raw cosine. 0.62 sits at search's own
 *  strictness; lower keeps a bit more, higher only what stands out most for this query. */
const THRESHOLD_MIN = 0.4;
const THRESHOLD_MAX = 0.9;
const NEW_THRESHOLD = 0.62;
const sigOf = (c: Collection, gen: number) => `${c.query}|${c.threshold}|${c.graphics ? 1 : 0}|${(c.exclude ?? []).join("\n")}|${gen}`;

interface Draft {
  /** Name of the collection being edited; null = new. */
  orig: string | null;
  name: string;
  query: string;
  threshold: number;
  graphics: boolean;
  exclude: string[];
}

export function Collections({ backend, images, hidden, t1, gen, epoch, actions, onError }: {
  backend: Backend;
  /** The user's own pictures, app graphics left out (for the screenshots collection). */
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
        const r = await backend.libraryCollection(c.query, c.threshold, CAP, c.exclude ?? [], c.graphics ?? false);
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

  // A collection that vanished (deleted) closes its viewer.
  useEffect(() => {
    if (selName && list && !list.some((c) => c.name === selName)) setSelName(null);
  }, [list, selName]);

  const sel = list?.find((c) => c.name === selName) ?? null;

  const shots = useMemo(
    () => images.filter((i) => i.class === "screenshot"),
    [images],
  );
  const hitsOf = useCallback((c: Collection): GridItem[] => {
    if (c.rule === "screenshots") return shots.map((i) => ({ path: i.path, name: baseName(i.path), size: i.size, mtime: i.mtime }));
    const gone = new Set((c.exclude ?? []).map((p) => p.toLowerCase()));
    return (evals[c.name]?.hits ?? [])
      .filter((h) => !hidden.has(h.path) && !gone.has(h.path.toLowerCase()))
      .map((h) => ({
        path: h.path, name: baseName(h.path), size: h.size ?? null, mtime: h.mtime ?? null,
        score: h.score, snippet: h.snippet ?? null, page: h.page ?? null, boxes: h.boxes ?? null,
      }));
  }, [evals, hidden, shots]);

  /** Pictures and videos first (what the grid is good at), then everything a document icon stands for. */
  const sections: GallerySection[] = useMemo(() => {
    const items = sel ? hitsOf(sel) : [];
    if (!sel || sel.rule || items.length === 0) return [{ key: sel?.name ?? "", label: null, items }];
    const isPic = (i: GridItem) => ["image", "video"].includes(kindOf(i.name, false));
    const photos = items.filter(isPic), docs = items.filter((i) => !isPic(i));
    return [
      { key: `${sel.name}:photos`, label: photos.length ? "Photos" : null, items: photos },
      { key: `${sel.name}:docs`, label: docs.length ? "Documents" : null, items: docs },
    ].filter((s) => s.items.length > 0);
  }, [sel, hitsOf]);

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

  const ev = sel && !sel.rule ? evals[sel.name] : undefined;

  // "Not in this collection": the file (and its copies) leaves for good; the strip under the header undoes the last one.
  const [lastGone, setLastGone] = useState<{ coll: string; path: string } | null>(null);
  useEffect(() => { setLastGone(null); }, [selName]);
  const removeFrom = useCallback((item: GridItem) => {
    if (!sel || sel.rule || !list) return;
    const exclude = [...(sel.exclude ?? []).filter((p) => p.toLowerCase() !== item.path.toLowerCase()), item.path];
    setLastGone({ coll: sel.name, path: item.path });
    void save(list.map((c) => (c.name === sel.name ? { ...c, exclude } : c)));
  }, [sel, list, save]);
  const undoRemove = useCallback(() => {
    if (!lastGone || !list) return;
    setLastGone(null);
    void save(list.map((c) => (c.name === lastGone.coll ? { ...c, exclude: (c.exclude ?? []).filter((p) => p !== lastGone.path) } : c)));
  }, [lastGone, list, save]);
  const extra: GridExtra | undefined = useMemo(
    () => (sel && !sel.rule ? { label: "Not in this collection", key: "x", run: removeFrom } : undefined),
    [sel, removeFrom],
  );

  const emptyMsg = !sel ? null
    : sel.rule ? <><strong>No screenshots yet</strong><span>Screen captures appear here as they are indexed.</span></>
    : ev?.status === "loading" || !ev ? <><span className="spin" aria-label="Loading" /><span>Searching…</span></>
    : ev.status === "off" ? <><strong>Search by meaning is not set up</strong><span>Download the models in the popup (Ctrl+,) and this collection fills itself.</span></>
    : ev.status === "error" ? <><strong>Could not search</strong><span>{ev.error}</span></>
    : <><strong>Nothing matches yet</strong><span>Lower the strictness, or describe it another way: several short phrases separated by commas work better than one long sentence.</span></>;

  const count = (c: Collection) => {
    if (c.rule === "screenshots") return shots.length;
    const e = evals[c.name];
    if (!e || e.status !== "ready") return null;
    const gone = new Set((c.exclude ?? []).map((p) => p.toLowerCase()));
    return e.hits.filter((h) => !hidden.has(h.path) && !gone.has(h.path.toLowerCase())).length;
  };

  const submit = async () => {
    if (!draft || !list) return;
    const c: Collection = { name: draft.name, query: draft.query, threshold: draft.threshold };
    const old = draft.orig ? list.find((x) => x.name === draft.orig) : undefined;
    if (old?.rule) c.rule = old.rule;
    else {
      if (draft.graphics) c.graphics = true;
      if (draft.exclude.length) c.exclude = draft.exclude;
    }
    const next = draft.orig ? list.map((x) => (x.name === draft.orig ? c : x)) : [...list, c];
    if (await save(next)) { setSelName(c.name.trim()); setDraft(null); }
  };

  const del = async () => {
    if (!sel || !list) return;
    if (await save(list.filter((c) => c.name !== sel.name))) { setSelName(null); setConfirmDel(false); }
  };

  const newCollection = () => { setDraft({ orig: null, name: "", query: "", threshold: NEW_THRESHOLD, graphics: false, exclude: [] }); setFormErr(null); };
  const editCollection = (c: Collection) => {
    setDraft({ orig: c.name, name: c.name, query: c.query, threshold: c.threshold, graphics: c.graphics ?? false, exclude: c.exclude ?? [] });
    setFormErr(null);
  };

  const gridRef = useRef<HTMLDivElement>(null);
  // The card grid is a keyboard widget: arrows move between cards, Enter opens one, Ctrl+N starts a new collection.
  const onGridKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.code === "KeyN") { e.preventDefault(); newCollection(); return; }
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(e.key)) return;
    const el = gridRef.current;
    if (!el) return;
    const cards = Array.from(el.querySelectorAll<HTMLElement>(".coll"));
    const at = cards.indexOf(document.activeElement as HTMLElement);
    if (at < 0) return;
    e.preventDefault();
    const cols = Math.max(1, getComputedStyle(el).gridTemplateColumns.split(" ").length);
    const n = e.key === "ArrowLeft" ? at - 1 : e.key === "ArrowRight" ? at + 1 : e.key === "ArrowUp" ? at - cols
      : e.key === "ArrowDown" ? at + cols : e.key === "Home" ? 0 : cards.length - 1;
    cards[Math.max(0, Math.min(cards.length - 1, n))]?.focus();
  };

  // Esc inside a collection goes back to the card grid (the form and the gallery's preview handle their own Esc first).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.defaultPrevented || !sel || draft) return;
      if (t && /^(INPUT|TEXTAREA)$/.test(t.tagName)) return;
      if (e.key === "Escape" || (e.altKey && e.key === "ArrowLeft")) { e.preventDefault(); setSelName(null); }
      else if (e.key === "F2") { e.preventDefault(); editCollection(sel); }
      else if (e.key === "Delete" && !t?.closest(".lg")) { e.preventDefault(); if (confirmDel) void del(); else setConfirmDel(true); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sel, draft, confirmDel, list]);

  /** Up to four thumbnails for a card. */
  const peek = (c: Collection): GridItem[] => {
    if (c.rule === "screenshots") return shots.slice(0, 4).map((i) => ({ path: i.path, name: baseName(i.path), size: i.size, mtime: i.mtime }));
    const out: GridItem[] = [];
    const gone = new Set((c.exclude ?? []).map((p) => p.toLowerCase()));
    for (const h of evals[c.name]?.hits ?? []) {
      if (hidden.has(h.path) || gone.has(h.path.toLowerCase())) continue;
      out.push({ path: h.path, name: baseName(h.path), size: h.size ?? null, mtime: h.mtime ?? null });
      if (out.length === 4) break;
    }
    return out;
  };
  const blurb = (c: Collection) => (c.rule === "screenshots" ? "Everything captured from a screen" : c.query);

  const embedded = t1?.stats?.done;
  const back = () => { setSelName(null); setDraft(null); setFormErr(null); };

  if (draft) {
    const ideas = draft.orig ? [] : freshIdeas(list?.map((c) => c.name) ?? []);
    return (
      <div className="page-body co">
        <form className="co-form" onSubmit={(e) => { e.preventDefault(); void submit(); }}
              onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setDraft(null); setFormErr(null); } }}>
          <h2>{draft.orig ? "Edit collection" : "New collection"}</h2>
          <label>Name<input autoFocus value={draft.name} maxLength={60} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="Receipts" /></label>
          {!(draft.orig && list?.find((c) => c.name === draft.orig)?.rule) && (
            <>
              <label>Describe what to find<input value={draft.query} onChange={(e) => setDraft({ ...draft, query: e.target.value })} placeholder="a paper receipt, an invoice, a utility bill" />
                <small>Name things you could see in the picture. Several kinds? Separate them with commas: each one is searched on its own.</small>
              </label>
              {ideas.length > 0 && (
                <div className="co-ideas" role="group" aria-label="Ideas">
                  {ideas.map((i) => (
                    <button type="button" key={i.name} className={"co-idea" + (draft.query === i.query ? " on" : "")} title={i.query}
                            onClick={() => setDraft({ ...draft, name: i.name, query: i.query })}>{i.name}</button>
                  ))}
                </div>
              )}
              <label>Strictness: <b>{draft.threshold.toFixed(2)}</b>
                <input type="range" min={THRESHOLD_MIN} max={THRESHOLD_MAX} step={0.01} value={draft.threshold} onChange={(e) => setDraft({ ...draft, threshold: Number(e.target.value) })} />
              </label>
              <label className="co-check">
                <input type="checkbox" checked={draft.graphics} onChange={(e) => setDraft({ ...draft, graphics: e.target.checked })} />
                <span>Include icons and app graphics<small>Off: only your photos, screenshots and documents.</small></span>
              </label>
              {draft.exclude.length > 0 && (
                <div className="co-gone">
                  <span>You removed {draft.exclude.length.toLocaleString("en-US")} file{draft.exclude.length === 1 ? "" : "s"} from this collection.</span>
                  <button type="button" className="btn sm" onClick={() => setDraft({ ...draft, exclude: [] })}>Let them back in</button>
                </div>
              )}
            </>
          )}
          {formErr && <div className="co-err" role="alert">{formErr}</div>}
          <div className="co-btns">
            <button type="submit" className="btn pri">Save</button>
            <button type="button" className="btn" onClick={() => { setDraft(null); setFormErr(null); }}>Cancel</button>
          </div>
        </form>
      </div>
    );
  }

  if (sel) {
    return (
      <div className="co co-view">
        <header className="co-head">
          <button className="btn co-back" onClick={back} title="Back to collections (Esc)"><IChevL size={14} />Collections</button>
          <div className="co-title">
            <h2>{sel.name}</h2>
            <p title={sel.rule ? undefined : sel.query}>{sel.rule === "screenshots" ? "Everything captured from a screen." : `“${sel.query}”`}</p>
          </div>
          {!sel.rule && (
            <label className="co-thr" title="How far a file must stand out from your library to belong here (the calibrated gate search uses). Higher = fewer, closer fits.">
              Strictness <b>{(thr ?? sel.threshold).toFixed(2)}</b>
              <input type="range" min={THRESHOLD_MIN} max={THRESHOLD_MAX} step={0.01} value={thr ?? sel.threshold} onChange={(e) => setThr(Number(e.target.value))} />
            </label>
          )}
          <div className="co-btns">
            <button className="btn" onClick={() => editCollection(sel)}>Edit</button>
            {confirmDel ? (
              <>
                <button className="btn pri" onClick={() => void del()}>Delete collection</button>
                <button className="btn" onClick={() => setConfirmDel(false)}>Keep</button>
              </>
            ) : (
              <button className="btn" onClick={() => setConfirmDel(true)}>Delete</button>
            )}
          </div>
        </header>
        {formErr && <div className="co-err" role="alert">{formErr}</div>}
        {lastGone && lastGone.coll === sel.name && (
          <div className="co-undo" role="status">
            <span>Removed “{baseName(lastGone.path)}” from {sel.name}. It will not come back.</span>
            <button className="btn sm" onClick={undoRemove}>Undo</button>
          </div>
        )}
        <div className="co-grid">
          <Gallery backend={backend} sections={sections} actions={actions} empty={emptyMsg} preview extra={extra} noun="item" />
        </div>
      </div>
    );
  }

  return (
    <div className="page-body co">
      <div className="colls" ref={gridRef} onKeyDown={onGridKey} role="list" aria-label="Smart collections">
        {list?.map((c, k) => {
          const n = count(c);
          const th = peek(c);
          return (
            <button key={c.name} role="listitem" className="coll" data-autofocus={k === 0 ? "" : undefined} onClick={() => { setSelName(c.name); setDraft(null); }}>
              <div className="c4">
                {[0, 1, 2, 3].map((j) => (
                  <div key={j}>{th[j] && <CollThumb backend={backend} item={th[j]} />}</div>
                ))}
              </div>
              <h4>{c.name}<span>{n === null ? (evals[c.name]?.status === "loading" ? "…" : "–") : n.toLocaleString("en-US")}</span></h4>
              <p title={blurb(c)}>{blurb(c)}</p>
            </button>
          );
        })}
        <button className="coll new" role="listitem" onClick={newCollection} data-autofocus={list?.length ? undefined : ""}>
          <div className="c4"><IPlus /></div>
          <h4>New collection</h4>
          <p>Type a description, like “dog photos”</p>
        </button>
      </div>
      {formErr && <div className="co-err" role="alert">{formErr}</div>}
      <p className="co-note">Each collection is a saved description: winseek keeps it filled from your photos, screenshots and documents (icons and app graphics stay out). Press X on anything that does not belong and it stays out. {embedded !== undefined && `${embedded.toLocaleString("en-US")} files understood so far.`}</p>
    </div>
  );
}

function CollThumb({ backend, item }: { backend: Backend; item: GridItem }) {
  const src = backend.iconUrl(item.path, false, true, 256);
  const [bad, setBad] = useState(false);
  if (!src || bad) return null;
  return <img src={src} alt="" draggable={false} decoding="async" onError={() => setBad(true)} />;
}
