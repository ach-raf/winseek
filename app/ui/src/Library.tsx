import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Backend, LibImage, VolumeStatus } from "./types";
import { TIMELINE_VIEWS, countViews, inView, parseTimelineView, type TimelineView } from "./libraryLogic";
import { Gallery, type GallerySection, type LibActions } from "./Gallery";
import { Collections } from "./Collections";
import { Cleanup } from "./Cleanup";
import { Sidebar, type Page } from "./Sidebar";
import { Settings, type SettingsSection } from "./Settings";
import { parseSection } from "./settingsNav";
import { baseName } from "./Semantic";
import { useTier1, useModels } from "./hooks";
import { groupByMonth } from "./libraryLogic";
import { combos, matches } from "./keymap";
import { Shortcuts } from "./Shortcuts";

export type LibTab = "timeline" | "collections" | "cleanup";
const TABS: LibTab[] = ["timeline", "collections", "cleanup"];
const TITLES: Record<LibTab, string> = { timeline: "Timeline", collections: "Collections", cleanup: "Clean up" };
const SUBS: Record<Exclude<LibTab, "timeline">, string> = {
  collections: "Describe what you want, winseek keeps it filled",
  cleanup: "Nothing is deleted until you say so",
};

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

export interface LibraryProps {
  backend: Backend;
  /** Rendered inside the full-window shell (the parent provides the Sidebar and the tab). */
  embedded?: boolean;
  /** Controlled tab when `embedded`. */
  tab?: LibTab;
  /** Window controls, shown at the right end of the top bar. */
  chrome?: ReactNode;
  /** Esc / Alt+Left (embedded). */
  onBack?: () => void;
}

export default function Library({ backend, embedded = false, tab: tabProp, chrome, onBack }: LibraryProps) {
  const [ownTab, setOwnTab] = useState<LibTab>(() => {
    const t = new URLSearchParams(location.search).get("tab");
    return TABS.includes(t as LibTab) ? (t as LibTab) : "timeline";
  });
  const tab: LibTab = embedded ? (tabProp ?? "timeline") : ownTab;
  const [epoch, setEpoch] = useState(0); // bumped after an undo: every list is read again
  // Settings inside this window: the sidebar's Settings item used to bounce back to the popup.
  const [showSettings, setShowSettings] = useState(false);
  // The last section is remembered across sessions (the popup uses the same storage key).
  const [settingsSection, setSettingsSectionRaw] = useState<SettingsSection>(() => {
    try { return parseSection(localStorage.getItem("winseek.settingsSection")); } catch { return "general"; }
  });
  const setSettingsSection = useCallback((s: SettingsSection) => {
    setSettingsSectionRaw(s);
    try { localStorage.setItem("winseek.settingsSection", s); } catch { /* remembered only when storage works */ }
  }, []);
  const [t1, refreshT1] = useTier1(backend, showSettings, epoch);
  const models = useModels(backend, refreshT1);
  const [images, setImages] = useState<LibImage[] | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  // Files sent to the Recycle Bin from here. They vanish from every list at once; an undo brings them back.
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  // What the timeline shows: your pictures by default; app graphics only when asked for.
  const [tlView, setTlViewRaw] = useState<TimelineView>(() => {
    try { return parseTimelineView(localStorage.getItem("winseek.timelineView")); } catch { return "all"; }
  });
  const setTlView = useCallback((v: TimelineView) => {
    setTlViewRaw(v);
    try { localStorage.setItem("winseek.timelineView", v); } catch { /* remembered only when storage works */ }
  }, []);
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const undoRef = useRef<string[] | null>(null);
  const [cheat, setCheat] = useState(false); // F1 / Ctrl+/
  const [status, setStatus] = useState<VolumeStatus[]>([]);

  const done = t1?.stats?.done ?? 0;
  const gen = useSettled(done, 20000);

  useEffect(() => {
    if (embedded) return;
    document.title = "winseek Library";
  }, [embedded]);

  // The standalone window has its own sidebar, so it reads the volume status itself (shallow-compared).
  useEffect(() => {
    if (embedded) return;
    let live = true;
    const poll = () => backend.status().then((st) => {
      if (live) setStatus((prev) => (JSON.stringify(prev) === JSON.stringify(st) ? prev : st));
    }).catch(() => {});
    poll();
    const t = window.setInterval(poll, 2000);
    return () => { live = false; window.clearInterval(t); };
  }, [backend, embedded]);

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

  // Settings' indexing rows: the same thin backend wrappers the popup passes (single mutation path).
  const setFolders = useCallback((include: string[]) => {
    backend.tier1SetScopes(include, t1?.exclude ?? []).then(refreshT1).catch(fail);
  }, [backend, t1, refreshT1, fail]);
  const removeFolder = useCallback((path: string) => {
    backend.tier1RemoveFolder(path).then(refreshT1).catch(fail);
  }, [backend, refreshT1, fail]);
  const pauseIndexing = useCallback((p: boolean) => {
    backend.tier1Pause(p).then(refreshT1).catch(fail);
  }, [backend, refreshT1, fail]);

  // Back to the search: embedded, the parent switches page; standalone, the popup shows first, then this window hides.
  const goBack = useCallback(() => {
    if (embedded) { onBack?.(); return; }
    backend.openPopup().then(() => backend.hide()).catch(fail);
  }, [backend, embedded, onBack, fail]);

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
      if (showSettings && (e.key === "Escape" || matches(e, "Alt+Left"))) { e.preventDefault(); setShowSettings(false); return; }
      if (!embedded && e.ctrlKey && !e.shiftKey && !e.altKey) {
        const ti = TABS.findIndex((_, i) => matches(e, `Ctrl+${i + 1}`));
        if (ti >= 0) { e.preventDefault(); setOwnTab(TABS[ti]); return; }
      }
      // Back to the search — unless something inside (preview, collection, drill-in, form, text field) owns the key.
      if (e.key === "Escape" || matches(e, "Alt+Left")) {
        if (e.defaultPrevented || t?.closest?.(".co-form") || (t && /^(INPUT|TEXTAREA)$/.test(t.tagName))) return;
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
  }, [doUndo, goBack, cheat, embedded, showSettings]);

  // A tab switch puts the keyboard on the tab's main widget (gallery, first collection, cleanup list).
  useEffect(() => {
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(".lib-body [data-autofocus], .lib-body [tabindex='0']")?.focus({ preventScroll: true });
    });
  }, [tab]);

  const visible = useMemo(() => (images ?? []).filter((i) => !hidden.has(i.path)), [images, hidden]);
  // Your own pictures: what collections and clean-up work on (app graphics are never offered there).
  const personal = useMemo(() => visible.filter((i) => i.class !== "graphic"), [visible]);
  const viewCounts = useMemo(() => countViews(visible), [visible]);
  const shown = useMemo(() => visible.filter((i) => inView(i.class, tlView)), [visible, tlView]);
  const sections: GallerySection[] = useMemo(
    () => groupByMonth(shown).map((s) => ({
      key: s.key, label: s.label, year: s.year, month: s.month,
      items: s.items.map((i) => ({ path: i.path, name: baseName(i.path), size: i.size, mtime: i.mtime })),
    })),
    [shown],
  );

  const emptyTimeline = images === null
    ? <><span className="spin" aria-label="Loading" /></>
    : loadErr
      ? <><strong>Could not read the photo index</strong><span>{loadErr}</span></>
      : t1?.state === "models_missing"
        ? <><strong>Nothing indexed yet</strong><span>Photos appear here once search by meaning is set up (Ctrl+, in the popup).</span></>
        : visible.length > 0
          ? <><strong>Nothing in this view</strong><span>{tlView === "graphics" ? "No icons or app graphics were found in your folders." : "Switch the view above to see what else was indexed."}</span></>
          : <><strong>No photos indexed yet</strong><span>Pictures, Desktop and Downloads are indexed in the background; they show up as they are read.</span></>;

  const sub = tab === "timeline"
    ? (images === null ? "" : tlView === "graphics"
      ? `${shown.length.toLocaleString("en-US")} icons and app graphics`
      : `${shown.length.toLocaleString("en-US")} ${tlView === "screenshots" ? "screenshots" : tlView === "photos" ? "photos" : "photos and screenshots"}`)
    : SUBS[tab];

  const onPage = (p: Page) => {
    if (p === "settings") { setShowSettings(true); return; }
    setShowSettings(false);
    if ((TABS as string[]).includes(p)) setOwnTab(p as LibTab);
    else goBack(); // "search": back to the popup
  };

  const main = (
    <div className="lib-main">
      <div className="tbar">
        <h1>{TITLES[tab]}</h1>
        {sub && <span className="hsub">{sub}</span>}
        <span className="sp" />
        {tab === "timeline" && images !== null && visible.length > 0 && (
          <div className="dseg tl-seg" role="tablist" aria-label="What the timeline shows">
            {TIMELINE_VIEWS.map((v) => (
              <button key={v.id} role="tab" aria-selected={tlView === v.id} className={tlView === v.id ? "on" : ""} title={v.hint} onClick={() => setTlView(v.id)}>
                {v.label}<i>{viewCounts[v.id].toLocaleString("en-US")}</i>
              </button>
            ))}
          </div>
        )}
        {chrome}
      </div>
      <main className="lib-body">
        {tab === "timeline" && <Gallery backend={backend} sections={sections} rail actions={actions} empty={emptyTimeline} />}
        {tab === "collections" && (
          <Collections backend={backend} images={personal} hidden={hidden} t1={t1} gen={gen} epoch={epoch} actions={actions} onError={fail} />
        )}
        {tab === "cleanup" && <Cleanup backend={backend} images={personal} hidden={hidden} actions={actions} epoch={epoch} gen={gen} onError={fail} />}
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

  if (embedded) return main;
  return (
    <div className="lib">
      <Sidebar page={showSettings ? "settings" : tab} onPage={onPage} status={status} />
      <div className="main-col">
        {showSettings ? (
          <Settings backend={backend} t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing}
                    onSetup={goBack} onClose={() => setShowSettings(false)} onHide={() => void backend.hide()}
                    onLibrary={() => setShowSettings(false)}
                    section={settingsSection} onSection={setSettingsSection} onError={fail} embedded escHint="back to the library" />
        ) : main}
      </div>
    </div>
  );
}
