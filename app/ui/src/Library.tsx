import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Backend } from "./types";
import { TIMELINE_VIEWS, parseGalleryView, parseTimelineView, type GalleryView, type TimelineView } from "./libraryLogic";
import { qk, refreshLibraryAfterUndo, refreshLibraryEmbeddings } from "./qk";
import { fsDelta } from "./fsDelta";
import { pushLocalDelta } from "./fsDeltaPatch";
import { useFsDelta } from "./useFsDelta";
import { Gallery, GalleryViewToggle, type GallerySection, type LibActions } from "./Gallery";
import { usePictureSearch } from "./usePictureSearch";
import { useTimelineSource } from "./timelineSource";
import { NameFilter } from "./NameFilter";
import { Collections } from "./Collections";
import { Cleanup } from "./Cleanup";
import { Sidebar, type Page } from "./Sidebar";
import { Settings } from "./Settings";
import { useTier1, useModels, useSettled, useStoredSection, useStoredValue, useDebouncedValue } from "./hooks";
import { combos, matches } from "./keymap";
import { Shortcuts } from "./Shortcuts";
import { useConfirm } from "./Confirm";
import { errText } from "./format";
import { recycleAsk } from "./confirmLogic";

export type LibTab = "timeline" | "collections" | "cleanup";
const TABS: LibTab[] = ["timeline", "collections", "cleanup"];
const TITLES: Record<LibTab, string> = { timeline: "Timeline", collections: "Collections", cleanup: "Clean up" };
const SUBS: Record<Exclude<LibTab, "timeline">, string> = {
  collections: "Describe what you want, winseek keeps it filled",
  cleanup: "Nothing is deleted until you say so",
};

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
  /** Clean up > Disk space should show this drive (a click on a drive in the side rail). */
  drive?: { letter: string; n: number };
  /** The Library is the visible page. Embedded, it stays mounted while other pages show (keep-alive);
   *  false mutes its window key handling and autofocus. Data still refreshes: `reveal` bumps on show. */
  active?: boolean;
}

export default function Library({ backend, embedded = false, tab: tabProp, chrome, onBack, drive, active = true }: LibraryProps) {
  const [ownTab, setOwnTab] = useState<LibTab>(() => {
    const t = new URLSearchParams(location.search).get("tab");
    return TABS.includes(t as LibTab) ? (t as LibTab) : "timeline";
  });
  const tab: LibTab = embedded ? (tabProp ?? "timeline") : ownTab;
  const [epoch, setEpoch] = useState(0); // bumped after an undo: the background scans (Similar / Disk space) start over
  // Settings inside this window: the sidebar's Settings item used to bounce back to the popup.
  const [showSettings, setShowSettings] = useState(false);
  // The last section is remembered across sessions (one hook with the popup; same storage key).
  const [settingsSection, setSettingsSection] = useStoredSection();
  const [t1, refreshT1] = useTier1(backend, showSettings);
  const models = useModels(backend, refreshT1);
  // The Library's own data, owned by react-query: cached across the keep-alive (tab switches and
  // page round-trips never refetch), re-read only when the inputs change (see the effects below).
  const qc = useQueryClient();
  // The timeline's photo list lives in the backend (a million photos as page objects cost ~2 GB): the page asks for a
  // load, then for a view of it (tab, name filter, hidden files) and gets months with counts; rows come by index
  // from the gallery as it scrolls (timelineSource).
  const baseQ = useQuery({ queryKey: qk.libTimelineBase, queryFn: () => backend.libraryTimelineLoad() });
  // Files sent to the Recycle Bin from here. They vanish from every list at once; an undo brings them back.
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  // What the timeline shows: your pictures by default; app graphics only when asked for.
  const [tlView, setTlView] = useStoredValue<TimelineView>("winseek.timelineView", parseTimelineView, (v) => v);
  // Tiles or list rows for the timeline gallery (per-screen, remembered).
  const [gview, setGview] = useStoredValue("winseek.galleryView.timeline", parseGalleryView, (v: GalleryView) => v);
  // What the timeline's search field keeps. It runs the popup's picture search (usePictureSearch: names + meaning,
  // ranked); the dated view underneath is never filtered by it.
  const [photoQ, setPhotoQ] = useState("");
  const searchQ = useDebouncedValue(photoQ.trim(), 150);
  const hiddenList = useMemo(() => [...hidden], [hidden]);
  const load = baseQ.data;
  const viewQ = useQuery({
    queryKey: qk.libTimeline(load ?? -1, tlView, "", hiddenList.join("\n")),
    queryFn: async () => {
      const r = await backend.libraryTimelineView(load!, tlView, "", hiddenList);
      if (!r) throw new Error("The photo list changed while it was being read");
      return r;
    },
    enabled: load !== undefined,
    placeholderData: keepPreviousData,
    gcTime: 0,
    staleTime: 0,
  });
  const summary = viewQ.data ?? null;
  const source = useTimelineSource(backend, summary);
  const loadErr = baseQ.isError ? errText(baseQ.error) : viewQ.isError && !summary ? errText(viewQ.error) : null;
  // State, not a ref, and marked during render: the newly picked tab must mount in THIS commit — a
  // ref flip would wait for the next unrelated render, leaving the body empty for seconds.
  const [seen, setSeen] = useState<Record<LibTab, boolean>>({ timeline: true, collections: false, cleanup: false });
  if (!seen[tab]) setSeen((s) => ({ ...s, [tab]: true }));
  // Whole-list consumers (Collections, Clean up) read the plain list, and only once one of their tabs was opened.
  const imagesQ = useQuery({ queryKey: qk.libImages, queryFn: () => backend.libraryImages(), enabled: seen.collections || seen.cleanup });
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const undoRef = useRef<string[] | null>(null);
  const [cheat, setCheat] = useState(false); // F1 / Ctrl+/
  // The "are you sure" gate for destructive actions: every recycle in this window asks here first.
  // Destructured so callers depend on the stable functions, not the fresh-each-render object.
  const { open: confirmOpen, askIt: askConfirm, dismiss: dismissConfirm, node: confirmNode } = useConfirm();

  const done = t1?.stats?.done ?? 0;
  const gen = useSettled(done, 20000);
  const search = usePictureSearch(backend, searchQ, { enabled: seen.timeline, semantic: !!t1 && t1.state !== "unavailable", epoch: gen });
  const searching = search.query !== "";
  // The ranked results as one flat section (no months); files recycled from here drop out at once.
  const results: GallerySection[] = useMemo(() => [{
    key: "results",
    label: null,
    items: search.items.filter((p) => !hidden.has(p.hit.path)).map((p) => ({
      path: p.hit.path, name: p.hit.name, size: p.hit.size, mtime: p.hit.mtime,
      score: p.sem?.score, snippet: p.sem?.snippet ?? null, page: p.sem?.page ?? null, at: p.sem?.at ?? null, boxes: p.sem?.boxes ?? null,
    })),
  }], [search.items, hidden]);

  useEffect(() => {
    if (embedded) return;
    document.title = "winseek Library";
  }, [embedded]);

  // The standalone window has its own sidebar, so it reads the volume status itself (polled; react-query's
  // structural sharing keeps unchanged polls from re-rendering, the old shallow JSON compare did this by hand).
  const statusQ = useQuery({ queryKey: qk.libStatus, queryFn: () => backend.status(), refetchInterval: 2000, enabled: !embedded });
  const status = statusQ.data ?? [];

  // Files deleted / renamed / added elsewhere (the search popup, Explorer) reach the lists as fs-deltas that
  // patch the cache in place (useFsDelta) — no re-read of the whole index on every reveal any more. Embedded,
  // the popup's App owns the window's subscription; the standalone window owns its own.
  useFsDelta(backend, !embedded);
  // New embeddings arrived (the settled done-count moved): picture list and dup groups re-read;
  // collections re-key themselves because the generation is part of their sig.
  const gen0 = useRef(gen);
  useEffect(() => { if (gen !== gen0.current) refreshLibraryEmbeddings(qc); }, [gen, qc]);

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
    // Every cached list (pictures, duplicates, biggest files, collections, Space) drops them at once; the
    // timeline's hidden set already covers its view, so its base list is not re-loaded.
    pushLocalDelta(qc, fsDelta({ removed: paths }), { timelineCovered: true });
    undoRef.current = paths;
    flash(paths.length === 1 ? "Moved to Recycle Bin" : `Moved ${paths.length} files to Recycle Bin`, true);
  }, [flash, qc]);

  const doUndo = useCallback(async () => {
    const paths = undoRef.current;
    if (!paths) return;
    undoRef.current = null;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    try {
      const n = await backend.restoreRecycled(paths);
      setHidden((h) => { const x = new Set(h); paths.forEach((p) => x.delete(p)); return x; });
      setEpoch((e) => e + 1); // the background scans (Similar / Disk space) start over
      refreshLibraryAfterUndo(qc); // index lists + every collection's evaluated hits are read again
      flash(n > 0 ? "Restored" : "Nothing to restore");
    } catch (e) { fail(e); }
  }, [backend, flash, fail, qc]);

  const actions: LibActions = useMemo(() => ({
    open: (p) => { backend.openPath(p).catch(fail); },
    reveal: (p) => { backend.revealPath(p).catch(fail); },
    copy: (p) => { backend.copyPath(p).then(() => flash("Path copied")).catch(fail); },
    menu: (p, x, y) => {
      backend.showContextMenu(p, x, y).then((r) => { if (r === "deleted") trashed([p]); }).catch(fail);
    },
    recycle: async (paths) => {
      if (paths.length === 0) return false;
      // The are-you-sure gate: single hit or whole batch, nothing recycles before it is answered.
      if (!(await askConfirm(recycleAsk(paths)))) return false;
      try {
        await backend.recycle(paths);
        trashed(paths);
        return true;
      } catch (e) { fail(e); return false; }
    },
  }), [backend, fail, flash, trashed, askConfirm]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!active) return; // hidden behind another page (embedded keep-alive): no keys to own
      const t = e.target as HTMLElement | null;
      if (combos("library.cheatsheet").some((c) => matches(e, c))) { e.preventDefault(); setCheat((v) => !v); return; }
      if (cheat && e.key === "Escape") { e.preventDefault(); setCheat(false); return; }
      // An open confirm dialog owns every key until it is answered (focus starts on Cancel, so
      // Enter answers no; Tab+Enter or a click go through). Esc / Alt+Left answer no as well.
      if (confirmOpen) {
        if (e.key === "Escape" || matches(e, "Alt+Left")) { e.preventDefault(); dismissConfirm(); }
        return;
      }
      if (showSettings && (e.key === "Escape" || matches(e, "Alt+Left"))) {
        if (t?.closest?.("dialog")) return; // a confirm dialog inside Settings owns the key until it is answered
        e.preventDefault(); setShowSettings(false); return;
      }
      if (!embedded && e.ctrlKey && !e.shiftKey && !e.altKey) {
        const ti = TABS.findIndex((_, i) => matches(e, `Ctrl+${i + 1}`));
        if (ti >= 0) { e.preventDefault(); setOwnTab(TABS[ti]!); return; }
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
  }, [doUndo, goBack, cheat, embedded, showSettings, confirmOpen, dismissConfirm, active]);

  // A tab switch puts the keyboard on the tab's main widget (gallery, first collection, cleanup list).
  useEffect(() => {
    if (!active) return;
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(".lib-body [data-active] [data-autofocus], .lib-body [data-active] [tabindex='0']")?.focus({ preventScroll: true });
    });
  }, [tab, active]);

  // Your own pictures: what collections and clean-up work on (app graphics are never offered there).
  const personal = useMemo(
    () => (imagesQ.data && !imagesQ.isError ? imagesQ.data.filter((i) => i.class !== "graphic" && !hidden.has(i.path)) : []),
    [imagesQ.data, imagesQ.isError, hidden],
  );
  const counts = summary?.counts ?? { all: 0, photos: 0, screenshots: 0, graphics: 0 };
  const visibleTotal = counts.all + counts.graphics; // everything indexed and not recycled, whatever the tab
  const shownTotal = summary?.total ?? 0;

  const emptyTimeline = summary === null && !loadErr
    ? <><span className="spin" aria-label="Loading" /></>
    : loadErr
      ? <><strong>Could not read the photo index</strong><span>{loadErr}</span></>
      : t1?.state === "models_missing"
        ? <><strong>Nothing indexed yet</strong><span>Photos appear here once search by meaning is set up (Ctrl+, in the popup).</span></>
        : visibleTotal > 0
          ? <><strong>Nothing in this view</strong><span>{tlView === "graphics" ? "No icons or app graphics were found in your folders." : "Switch the view above to see what else was indexed."}</span></>
          : <><strong>No photos indexed yet</strong><span>Pictures, Desktop and Downloads are indexed in the background; they show up as they are read.</span></>;

  const sub = tab === "timeline"
    ? (summary === null ? "" : tlView === "graphics"
      ? `${shownTotal.toLocaleString("en-US")} icons and app graphics`
      : `${shownTotal.toLocaleString("en-US")} ${tlView === "screenshots" ? "screenshots" : tlView === "photos" ? "photos" : "photos and screenshots"}`)
    : SUBS[tab];

  const onPage = (p: Page) => {
    if (p === "settings") { setShowSettings(true); return; }
    setShowSettings(false);
    if ((TABS as string[]).includes(p)) setOwnTab(p as LibTab);
    else goBack(); // "search": back to the popup
  };

  // A visited tab stays mounted (hidden): switching back neither refetches its data nor loses its
  // place — Cleanup keeps its dup groups and drill-in, Collections keeps its evaluated hits and the
  // open collection, the gallery keeps its scroll. The reveal effect above re-reads what can change
  // outside (via the react-query cache).
  // State, not a ref, and marked during render: the newly picked tab must mount in THIS commit — a
  // ref flip would wait for the next unrelated render, leaving the body empty for seconds.
  const keep = (t: LibTab, node: ReactNode) => (
    <div className="lib-keep" hidden={tab !== t} data-active={tab === t ? "" : undefined}>{node}</div>
  );

  const main = (
    <div className="lib-main">
      <div className="tbar">
        <h1>{TITLES[tab]}</h1>
        {sub && <span className="hsub">{sub}</span>}
        <span className="sp" />
        {tab === "timeline" && summary !== null && visibleTotal > 0 && (
          <NameFilter value={photoQ} onChange={setPhotoQ} label="Search pictures" placeholder="Search pictures" />
        )}
        {chrome}
      </div>
      {tab === "timeline" && summary !== null && visibleTotal > 0 && (
        <div className="tl-bar">
          <div className="dseg tl-seg" role="tablist" aria-label="What the timeline shows">
            {TIMELINE_VIEWS.map((v) => (
              <button key={v.id} role="tab" aria-selected={tlView === v.id} className={tlView === v.id ? "on" : ""} title={v.hint} onClick={() => setTlView(v.id)}>
                {v.label}<i>{counts[v.id].toLocaleString("en-US")}</i>
              </button>
            ))}
          </div>
          <span className="sp" />
          <GalleryViewToggle view={gview} onChange={setGview} />
        </div>
      )}
      <main className="lib-body">
        {seen.timeline && keep("timeline", searching ? (
          <>
            <div className="lg-results" role="status">
              <span>Results for <b>{`“${search.query}”`}</b></span>
              <i>{search.total.toLocaleString("en-US")}{search.loading ? "…" : ""}</i>
              <span className="sp" />
              <button className="btn sm secondary" onClick={() => setPhotoQ("")} title="Back to the timeline (clears the search)">Clear</button>
            </div>
            <Gallery backend={backend} sections={results} listKey={`results|${search.query}`} actions={actions} view={gview} onMore={search.loadMore}
                     empty={search.loading
                       ? <><span className="spin" aria-label="Searching" /><span>Searching…</span></>
                       : <><strong>{`Nothing matches “${search.query}”`}</strong><span>Try other words, or describe what the picture shows.</span></>} />
          </>
        ) : <Gallery backend={backend} source={source} rail actions={actions} empty={emptyTimeline} view={gview} />)}
        {seen.collections && keep("collections", (
          <Collections backend={backend} images={personal} hidden={hidden} t1={t1} gen={gen} actions={actions} onError={fail} active={tab === "collections"} />
        ))}
        {seen.cleanup && keep("cleanup", (
          <Cleanup backend={backend} images={personal} hidden={hidden} actions={actions} epoch={epoch} onError={fail} drive={drive} active={tab === "cleanup"} />
        ))}
      </main>
      {cheat && <Shortcuts scope="library" onClose={() => setCheat(false)} />}
      {confirmNode}
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
