import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, LibImage } from "./types";
import { Gallery, type GallerySection, type LibActions } from "./Gallery";
import { Collections } from "./Collections";
import { Cleanup } from "./Cleanup";
import { baseName } from "./Semantic";
import { useTier1 } from "./hooks";
import { applyAccent } from "./accent";
import { groupByMonth } from "./libraryLogic";
import { tooltip } from "./Shortcuts";
import { combos, matches } from "./keymap";
import { Shortcuts } from "./Shortcuts";

type Tab = "timeline" | "collections" | "cleanup";
const TABS: { id: Tab; label: string }[] = [
  { id: "timeline", label: "Timeline" },
  { id: "collections", label: "Collections" },
  { id: "cleanup", label: "Clean up" },
];

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

/** `value`, but a change shows up at most once per `ms` (the last one always does). */
function useSettled<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  const last = useRef(0);
  useEffect(() => {
    if (Object.is(v, value)) return;
    const t = window.setTimeout(() => { last.current = Date.now(); setV(value); }, Math.max(0, last.current + ms - Date.now()));
    return () => window.clearTimeout(t);
  }, [value, ms, v]);
  return v;
}

export default function Library({ backend }: { backend: Backend }) {
  const [tab, setTab] = useState<Tab>(() => {
    const t = new URLSearchParams(location.search).get("tab");
    return TABS.some((x) => x.id === t) ? (t as Tab) : "timeline";
  });
  const [epoch, setEpoch] = useState(0); // bumped after an undo: every list is read again
  const [t1] = useTier1(backend, false, epoch);
  const [images, setImages] = useState<LibImage[] | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  // Files sent to the Recycle Bin from here. They vanish from every list at once; an undo brings them back.
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const undoRef = useRef<string[] | null>(null);
  const [cheat, setCheat] = useState(false); // F1 / Ctrl+/

  const embedded = t1?.stats?.done ?? 0;
  const gen = useSettled(embedded, 20000);

  useEffect(() => {
    document.title = "winseek Library";
    const dark = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => backend.accentPalette().then((p) => applyAccent(p, dark.matches)).catch(() => {});
    apply();
    dark.addEventListener("change", apply);
    return () => dark.removeEventListener("change", apply);
  }, [backend]);

  useEffect(() => {
    let live = true;
    backend.libraryImages().then((r) => { if (live) { setImages(r); setLoadErr(null); } }).catch((e) => { if (live) { setImages((x) => x ?? []); setLoadErr(errText(e)); } });
    return () => { live = false; };
  }, [backend, epoch, gen]);

  const flash = useCallback((msg: string, undo = false, ms = undo ? 7000 : 2500) => {
    setToast({ msg, undo });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), ms);
  }, []);
  const fail = useCallback((e: unknown) => flash(errText(e), false, 4500), [flash]);

  // Back to the popup: it shows first, then this window hides — the mirror of openLibrary in the popup.
  const goBack = useCallback(() => {
    backend.openPopup().then(() => backend.hide()).catch(fail);
  }, [backend, fail]);

  const trashed = useCallback((paths: string[]) => {
    setHidden((h) => { const n = new Set(h); paths.forEach((p) => n.add(p)); return n; });
    undoRef.current = paths;
    flash(paths.length === 1 ? "Moved to Recycle Bin" : `Moved ${paths.length} files to Recycle Bin`, true);
  }, [flash]);

  const doUndo = useCallback(async () => {
    const paths = undoRef.current;
    if (!paths) return;
    undoRef.current = null;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    try {
      const n = await backend.restoreRecycled(paths);
      setHidden((h) => { const x = new Set(h); paths.forEach((p) => x.delete(p)); return x; });
      setEpoch((e) => e + 1);
      flash(n > 0 ? "Restored" : "Nothing to restore");
    } catch (e) { fail(e); }
  }, [backend, flash, fail]);

  const actions: LibActions = useMemo(() => ({
    open: (p) => { backend.openPath(p).catch(fail); },
    reveal: (p) => { backend.revealPath(p).catch(fail); },
    copy: (p) => { backend.copyPath(p).then(() => flash("Path copied")).catch(fail); },
    menu: (p, x, y) => {
      backend.showContextMenu(p, x, y).then((r) => { if (r === "deleted") trashed([p]); }).catch(fail);
    },
    recycle: async (paths) => {
      if (paths.length === 0) return;
      try {
        await backend.recycle(paths);
        trashed(paths);
      } catch (e) { fail(e); }
    },
  }), [backend, fail, flash, trashed]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (combos("library.cheatsheet").some((c) => matches(e, c))) { e.preventDefault(); setCheat((v) => !v); return; }
      if (cheat && e.key === "Escape") { e.preventDefault(); setCheat(false); return; }
      if (e.ctrlKey && !e.shiftKey && !e.altKey) {
        const ti = TABS.findIndex((_, i) => matches(e, `Ctrl+${i + 1}`));
        if (ti >= 0) { e.preventDefault(); setTab(TABS[ti].id); return; }
      }
      // Back to the search — unless a collection editor or a text field owns the key.
      if (e.key === "Escape" || matches(e, "Alt+Left")) {
        if (t?.closest?.(".co-form") || (t && /^(INPUT|TEXTAREA)$/.test(t.tagName))) return;
        e.preventDefault();
        goBack();
        return;
      }
      if (e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "z" && undoRef.current && !(t && /^(INPUT|TEXTAREA)$/.test(t.tagName))) {
        e.preventDefault();
        void doUndo();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [doUndo, goBack, cheat]);

  // A tab switch puts the keyboard on the tab's main list (gallery, collections, cleanup groups).
  useEffect(() => {
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(".lib-body [tabindex='0']")?.focus({ preventScroll: true });
    });
  }, [tab]);

  const visible = useMemo(() => (images ?? []).filter((i) => !hidden.has(i.path)), [images, hidden]);
  const sections: GallerySection[] = useMemo(
    () => groupByMonth(visible).map((s) => ({
      key: s.key, label: s.label, year: s.year, month: s.month,
      items: s.items.map((i) => ({ path: i.path, name: baseName(i.path), size: i.size, mtime: i.mtime })),
    })),
    [visible],
  );

  const status = (() => {
    if (!t1) return "";
    const s = t1.stats;
    switch (t1.state) {
      case "unavailable": return "Photo index unavailable";
      case "models_missing": return "Search by meaning is off";
      case "indexing": return s ? `Indexing · ${s.done.toLocaleString("en-US")} of ${(s.done + s.pending).toLocaleString("en-US")} photos` : "Indexing";
      case "paused": return `Paused · ${(s?.done ?? 0).toLocaleString("en-US")} photos indexed`;
      default: return s ? `${s.done.toLocaleString("en-US")} photos indexed` : "";
    }
  })();

  const emptyTimeline = images === null
    ? <><span className="spin" aria-label="Loading" /></>
    : loadErr
      ? <><strong>Could not read the photo index</strong><span>{loadErr}</span></>
      : t1?.state === "models_missing"
        ? <><strong>Nothing indexed yet</strong><span>Photos appear here once search by meaning is set up (Ctrl+, in the popup).</span></>
        : <><strong>No photos indexed yet</strong><span>Pictures, Desktop and Downloads are indexed in the background; they show up as they are read.</span></>;

  return (
    <div className="lib">
      <header className="lib-top">
        <button className="lib-back" onClick={goBack} title={tooltip("library.back")} aria-label="Back to search">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
        </button>
        <h1>Library</h1>
        <nav className="lib-tabs" role="tablist">
          <div className="seg">
            {TABS.map((t) => (
              <button key={t.id} role="tab" aria-selected={tab === t.id} className={tab === t.id ? "on" : ""} onClick={() => setTab(t.id)}>{t.label}</button>
            ))}
          </div>
        </nav>
        <span className="spacer" />
        <span className={"lib-status s-" + (t1?.state ?? "idle")}><i className="dot" />{status}</span>
      </header>
      <main className="lib-body">
        {tab === "timeline" && <Gallery backend={backend} sections={sections} rail actions={actions} empty={emptyTimeline} />}
        {tab === "collections" && (
          <Collections backend={backend} images={visible} hidden={hidden} t1={t1} gen={gen} epoch={epoch} actions={actions} onError={fail} />
        )}
        {tab === "cleanup" && <Cleanup backend={backend} hidden={hidden} actions={actions} epoch={epoch} gen={gen} onError={fail} />}
      </main>
      {cheat && <Shortcuts scope="library" onClose={() => setCheat(false)} />}
      {toast && (
        <div className="lib-toast" role="status">
          {toast.msg}
          {toast.undo && <> · <button className="undo" onClick={() => void doUndo()}>Undo</button></>}
        </div>
      )}
    </div>
  );
}
