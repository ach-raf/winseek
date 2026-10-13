import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Filter, Hit, ServiceState, Sort, VolumeStatus } from "./types";
import { FirstRun, type Summary } from "./FirstRun";
import { Setup } from "./Setup";
import { Settings } from "./Settings";
import { SemRow, SemanticHint, baseName, dedupe, pathKey } from "./Semantic";
import { useModels, useSemantic, useTier1 } from "./hooks";
import { applyAccent } from "./accent";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize, kindOf, matchRanges, splitDir } from "./format";

const ROW_H = 54;
const PAGE = 80;
const OVERSCAN = 6;

const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "images", label: "Images" },
  { id: "docs", label: "Docs" },
  { id: "video", label: "Video" },
  { id: "audio", label: "Audio" },
  { id: "folders", label: "Folders" },
];
const SORTS: { id: Sort; label: string }[] = [
  { id: "rel", label: "Best match" },
  { id: "name", label: "Name" },
  { id: "date", label: "Date modified" },
  { id: "size", label: "Size" },
];

function Highlight({ text, query }: { text: string; query: string }) {
  const ranges = matchRanges(text, query);
  if (!ranges.length) return <>{text}</>;
  const out: React.ReactNode[] = [];
  let at = 0;
  ranges.forEach(([s, e], i) => {
    if (s > at) out.push(text.slice(at, s));
    out.push(<mark key={i}>{text.slice(s, e)}</mark>);
    at = e;
  });
  if (at < text.length) out.push(text.slice(at));
  return <>{out}</>;
}

function RenameInput({ initial, isDir, onCommit, onCancel }: {
  initial: string; isDir: boolean; onCommit: (name: string) => void; onCancel: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const dot = initial.lastIndexOf(".");
    const end = !isDir && dot > 0 ? dot : initial.length;
    const apply = () => { el.focus(); el.setSelectionRange(0, end); };
    apply();
    const t = window.setTimeout(apply, 0); // some engines re-select-all after a keyboard-initiated focus
    return () => window.clearTimeout(t);
  }, [initial, isDir]);
  const finish = (commit: boolean) => {
    if (done.current) return;
    done.current = true;
    if (commit) onCommit(ref.current!.value); else onCancel();
  };
  return (
    <input
      ref={ref}
      className="rename"
      defaultValue={initial}
      spellCheck={false}
      autoComplete="off"
      aria-label="New name"
      onKeyDown={(e) => {
        e.stopPropagation(); // global handler must not see keys while renaming
        if (e.key === "Enter") { e.preventDefault(); finish(true); }
        else if (e.key === "Escape") { e.preventDefault(); finish(false); }
      }}
      onBlur={() => finish(false)}
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
    />
  );
}

const Row = memo(function Row({
  hit, query, selected, top, icon, thumb, renaming, both, onPick, onHover, onDown, onMenu, onRenameCommit, onRenameCancel,
}: {
  hit: Hit | undefined; query: string; selected: boolean; top: number; icon: string | null; thumb: boolean; renaming: boolean;
  both: boolean; // also found by meaning
  onPick: () => void; onHover: () => void; onDown: (e: React.MouseEvent) => void; onMenu: (e: React.MouseEvent) => void;
  onRenameCommit: (name: string) => void; onRenameCancel: () => void;
}) {
  if (!hit) {
    return (
      <div className="row skeleton" style={{ transform: `translateY(${top}px)` }}>
        <span className="ficon" />
        <div className="meta"><span className="sk w1" /><span className="sk w2" /></div>
      </div>
    );
  }
  const { head, tail } = splitDir(hit.path);
  return (
    <div
      className={"row" + (selected ? " selected" : "")}
      style={{ transform: `translateY(${top}px)` }}
      role="option"
      aria-selected={selected}
      onClick={renaming ? undefined : onPick}
      onMouseMove={renaming ? undefined : onHover}
      onMouseDown={onDown}
      onContextMenu={onMenu}
      title={hit.path}
    >
      <FileIcon name={hit.name} isDir={hit.is_dir} src={icon} thumb={thumb} />
      <div className="meta">
        <div className="name">
          {renaming
            ? <RenameInput initial={hit.name} isDir={hit.is_dir} onCommit={onRenameCommit} onCancel={onRenameCancel} />
            : <><Highlight text={hit.name} query={query} />{both && <span className="chip-both">name + meaning</span>}</>}
        </div>
        <div className="path"><span className="head">{head}</span><span className="tail">{tail}</span></div>
      </div>
      <div className="side">
        <span className="size">{hit.is_dir ? "Folder" : formatSize(hit.size)}</span>
        <span className="date">{formatDate(hit.mtime)}</span>
      </div>
    </div>
  );
});

type Undo = { kind: "rename"; newPath: string; oldName: string } | { kind: "recycle"; paths: string[] };

interface Data { total: number; rows: (Hit | undefined)[]; elapsedMs: number | null; recent: boolean }

export default function App({ backend }: { backend: Backend }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("rel");
  const [sel, setSel] = useState(0);
  const [data, setData] = useState<Data>({ total: 0, rows: [], elapsedMs: null, recent: true });
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<VolumeStatus[]>([]);
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const [renaming, setRenaming] = useState(false); // inline rename of the selected row
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(360);
  const [epoch, setEpoch] = useState(0); // bumped when the popup is re-shown
  // First run: panel shown while on demo data with the service missing/stopped. Typing hides it (clearing the
  // box brings it back); "Not now" hides it for the session. After install/start it becomes a live counter that
  // ends on the first keystroke once everything is live.
  const [service, setService] = useState<ServiceState | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const [setupBusy, setSetupBusy] = useState(false);
  const [setupErr, setSetupErr] = useState<string | null>(null);
  const [counting, setCounting] = useState(false);
  // Search by meaning (Tier 1): setup panel, the "off" hint, semantic hits.
  const [setupOpen, setSetupOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [hintOff, setHintOff] = useState(false);
  const [summary, setSummary] = useState<Summary | null>(null);
  const t0 = useRef<number | null>(null);
  const sawScan = useRef(false);
  const showPanelRef = useRef(false);
  const summaryRef = useRef<Summary | null>(null);
  const primaryRef = useRef<() => void>(() => {});
  const setupOpenRef = useRef(false);
  const setupPrimaryRef = useRef<() => void>(() => {});
  const semRef = useRef<HTMLDivElement>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const seq = useRef(0);
  const inflight = useRef(new Set<number>());
  const rowsRef = useRef<(Hit | undefined)[]>([]);
  const toastTimer = useRef<number | undefined>(undefined);
  // One-level undo (last rename or recycle). Ctrl+Z triggers it only while the search box is empty or
  // unchanged since the action; otherwise Ctrl+Z is left to the browser (text undo).
  const undoRef = useRef<Undo | null>(null);
  const undoQuery = useRef("");
  const suppressClick = useRef(false);

  const isRecent = query.trim() === "" && filter === "all" && sort === "rel";

  // ---- search by meaning ---------------------------------------------------
  const [t1, refreshT1] = useTier1(backend, setupOpen || settingsOpen, epoch);
  const models = useModels(backend, refreshT1);
  const t1Off = !!t1 && (t1.state === "models_missing" || t1.state === "unavailable");
  const sem = useSemantic(backend, query, !!t1 && !t1Off && !isRecent, filter, epoch);
  const semKeys = useMemo(() => new Set(sem.hits.map((h) => pathKey(h.path))), [sem.hits]);
  // A file found by both stays where it is in the name results (they never move); only the others are listed here.
  // Dedupe looks at the first 400 name rows, the ones a user can plausibly be looking at.
  const semRows = useMemo(
    () => dedupe(sem.hits, data.rows.slice(0, 400).flatMap((h) => (h ? [h.path] : []))),
    [sem.hits, data.rows],
  );
  const selCount = data.total + semRows.length;

  // ---- data loading --------------------------------------------------------
  useEffect(() => {
    const my = ++seq.current;
    inflight.current.clear();
    (async () => {
      if (isRecent) {
        const hits = await backend.recent(40);
        if (my !== seq.current) return;
        rowsRef.current = hits;
        setData({ total: hits.length, rows: hits, elapsedMs: null, recent: true });
      } else {
        const r = await backend.search({ query, offset: 0, limit: PAGE, sort, filter });
        if (my !== seq.current) return;
        const rows: (Hit | undefined)[] = new Array(r.total).fill(undefined);
        r.hits.forEach((h, i) => (rows[i] = h));
        rowsRef.current = rows;
        setData({ total: r.total, rows, elapsedMs: r.elapsed_us / 1000, recent: false });
      }
      setLoaded(true);
      setSel(0);
      setScrollTop(0);
      if (listRef.current) listRef.current.scrollTop = 0;
    })().catch(console.error);
  }, [backend, query, filter, sort, isRecent, epoch]);

  const ensurePages = useCallback((first: number, last: number) => {
    if (data.recent) return;
    const my = seq.current;
    for (let p = Math.floor(first / PAGE); p <= Math.floor(last / PAGE); p++) {
      if (rowsRef.current[p * PAGE] || inflight.current.has(p) || p * PAGE >= data.total) continue;
      inflight.current.add(p);
      backend.search({ query, offset: p * PAGE, limit: PAGE, sort, filter }).then((r) => {
        if (my !== seq.current) return;
        const rows = rowsRef.current.slice();
        r.hits.forEach((h, i) => (rows[p * PAGE + i] = h));
        rowsRef.current = rows;
        setData((d) => ({ ...d, rows }));
      }).catch(console.error);
    }
  }, [backend, data.recent, data.total, query, sort, filter]);

  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(data.total - 1, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);
  useEffect(() => { if (last >= first) ensurePages(first, last); }, [first, last, ensurePages]);

  // ---- status / window events ---------------------------------------------
  const fast = counting && !summary; // 250 ms while the first scan is being watched
  const refreshStatus = useCallback(() => {
    backend.status().then((st) => {
      setStatus(st);
      if (st.length > 0 && st.every((v) => v.state === "demo")) backend.serviceState().then(setService).catch(() => {});
    }).catch(() => {});
  }, [backend]);
  useEffect(() => {
    refreshStatus();
    const t = window.setInterval(refreshStatus, fast ? 250 : 1000);
    return () => window.clearInterval(t);
  }, [refreshStatus, fast, epoch]);

  const startCounting = useCallback(() => {
    t0.current ??= performance.now();
    sawScan.current = false;
    setSummary(null);
    setCounting(true);
  }, []);

  useEffect(() => {
    let off: (() => void) | undefined;
    backend.onSourceChanged(() => {
      if (showPanelRef.current) startCounting();
      refreshStatus();
      setEpoch((e) => e + 1);
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend, refreshStatus, startCounting]);

  useEffect(() => {
    if (!counting || summary || !status.length) return;
    if (status.some((v) => v.state === "scanning")) { sawScan.current = true; t0.current ??= performance.now(); }
    if (status.every((v) => v.state === "live")) {
      const secs = sawScan.current && t0.current != null ? (performance.now() - t0.current) / 1000 : null;
      setSummary({ files: status.reduce((a, v) => a + v.files + v.folders, 0), letters: status.map((v) => v.letter), secs });
    }
  }, [counting, summary, status]);

  // accent colour follows Windows; re-read on load, popup show and light/dark switch
  useEffect(() => {
    const dark = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => backend.accentPalette().then((p) => applyAccent(p, dark.matches)).catch(() => {});
    apply();
    dark.addEventListener("change", apply);
    return () => dark.removeEventListener("change", apply);
  }, [backend, epoch]);

  useEffect(() => {
    let off: (() => void) | undefined;
    backend.onShown(() => {
      setEpoch((e) => e + 1);
      if (summaryRef.current) setCounting(false); // the "indexed N files" line is one-shot
      requestAnimationFrame(() => { inputRef.current?.focus(); inputRef.current?.select(); });
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend]);

  useEffect(() => { inputRef.current?.focus(); }, []);

  useLayoutEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // ---- actions -------------------------------------------------------------
  const flash = useCallback((msg: string, undo = false, ms = undo ? 5000 : 1600) => {
    setToast({ msg, undo });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), ms);
  }, []);
  const fail = useCallback((e: unknown) => {
    flash(String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, ""), false, 3500);
  }, [flash]);
  const refocus = useCallback(() => requestAnimationFrame(() => inputRef.current?.focus()), []);

  const semSel = sel >= data.total ? semRows[sel - data.total] : undefined;
  const current: Hit | undefined = sel < data.total
    ? data.rows[sel]
    : semSel && { name: baseName(semSel.path), path: semSel.path, is_dir: false, size: null, mtime: null };

  const act = useCallback(async (kind: "open" | "reveal" | "copy", h: Hit | undefined) => {
    if (!h) return;
    try {
      if (kind === "copy") { await backend.copyPath(h.path); flash("Path copied"); return; }
      if (kind === "open") await backend.openPath(h.path); else await backend.revealPath(h.path);
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, flash, fail]);

  const removeRow = useCallback((i: number) => {
    const rows = rowsRef.current.slice();
    rows.splice(i, 1);
    rowsRef.current = rows;
    setData((d) => ({ ...d, rows, total: Math.max(0, d.total - 1) }));
    setSel((s) => Math.max(0, Math.min(s, rows.length - 1)));
  }, []);

  const pushUndo = useCallback((u: Undo, msg: string) => {
    undoRef.current = u;
    undoQuery.current = inputRef.current?.value ?? "";
    flash(msg, true);
  }, [flash]);

  const doRecycle = useCallback(async (i: number) => {
    const h = rowsRef.current[i];
    if (!h) return;
    try {
      await backend.recycle([h.path]);
      removeRow(i);
      pushUndo({ kind: "recycle", paths: [h.path] }, "Moved to Recycle Bin");
    } catch (e) { fail(e); }
  }, [backend, removeRow, pushUndo, fail]);

  const doRename = useCallback(async (i: number, newName: string) => {
    setRenaming(false);
    refocus();
    const h = rowsRef.current[i];
    const name = newName.trim();
    if (!h || !name || name === h.name) return;
    try {
      const np = await backend.renamePath(h.path, name);
      const rows = rowsRef.current.slice();
      rows[i] = { ...h, name, path: np };
      rowsRef.current = rows;
      setData((d) => ({ ...d, rows }));
      pushUndo({ kind: "rename", newPath: np, oldName: h.name }, "Renamed");
    } catch (e) { fail(e); }
  }, [backend, pushUndo, fail, refocus]);

  const doUndo = useCallback(async () => {
    const u = undoRef.current;
    if (!u) return;
    undoRef.current = null;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    try {
      if (u.kind === "rename") {
        await backend.renamePath(u.newPath, u.oldName);
        flash("Rename undone");
      } else {
        const n = await backend.restoreRecycled(u.paths);
        flash(n > 0 ? "Restored" : "Nothing to restore");
      }
      setEpoch((e) => e + 1); // re-run the search to refresh the list
    } catch (e) { fail(e); }
  }, [backend, flash, fail]);

  const openMenu = useCallback(async (i: number, x: number, y: number) => {
    const h = rowsRef.current[i];
    if (!h) return;
    try {
      const r = await backend.showContextMenu(h.path, x, y);
      if (r === "rename") setRenaming(true);
      else if (r === "deleted") { removeRow(i); pushUndo({ kind: "recycle", paths: [h.path] }, "Moved to Recycle Bin"); }
      else if (r === "invoked") { await backend.hide(); return; } // Open / Open with / Send to... moved on to another app
    } catch (e) { fail(e); }
    refocus();
  }, [backend, removeRow, pushUndo, fail, refocus]);

  const onRowDown = useCallback((e: React.MouseEvent, h: Hit) => {
    suppressClick.current = false;
    if (e.button !== 0) return;
    const sx = e.clientX, sy = e.clientY;
    const stop = () => { window.removeEventListener("mousemove", mv); window.removeEventListener("mouseup", stop); };
    const mv = (ev: MouseEvent) => {
      if (Math.hypot(ev.clientX - sx, ev.clientY - sy) <= 4) return;
      stop();
      suppressClick.current = true; // the click that follows the drag must not open the file
      backend.startDrag([h.path]).catch(fail);
    };
    window.addEventListener("mousemove", mv);
    window.addEventListener("mouseup", stop);
  }, [backend, fail]);

  const move = useCallback((to: number) => {
    const n = Math.max(0, Math.min(selCount - 1, to));
    setSel(n);
    const el = listRef.current;
    if (!el || n >= data.total) return; // semantic rows scroll into view in an effect
    const top = n * ROW_H, bottom = top + ROW_H;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }, [data.total, selCount]);

  useEffect(() => {
    if (sel >= data.total) semRef.current?.querySelector('[data-sel="1"]')?.scrollIntoView({ block: "nearest" });
    setSel((s) => (s >= selCount ? Math.max(0, selCount - 1) : s));
  }, [sel, data.total, selCount]);

  const openSemMenu = useCallback(async (path: string, x: number, y: number) => {
    try {
      const r = await backend.showContextMenu(path, x, y);
      if (r === "invoked") { await backend.hide(); return; }
      if (r === "deleted") { flash("Moved to Recycle Bin"); setEpoch((e) => e + 1); }
    } catch (e) { fail(e); }
    refocus();
  }, [backend, flash, fail, refocus]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (renaming) return;
      if ((e.target as HTMLElement | null)?.closest?.(".setup")) return; // the setup panel's own inputs / buttons
      const inp = inputRef.current;
      const inInput = !!inp && document.activeElement === inp;
      const hasSelection = inInput && inp!.selectionStart !== inp!.selectionEnd;
      const atEnd = !inInput || (!hasSelection && inp!.selectionStart === inp!.value.length);
      const menuAtSelection = () => {
        const el = listRef.current;
        if (!el || !current) return;
        if (sel >= data.total) {
          const sr = semRef.current?.querySelector('[data-sel="1"]')?.getBoundingClientRect();
          if (sr) void openSemMenu(current.path, sr.left + 120, sr.top + sr.height / 2);
          return;
        }
        const r = el.getBoundingClientRect();
        void openMenu(sel, r.left + 120, r.top + 4 + sel * ROW_H - el.scrollTop + ROW_H / 2);
      };
      const page = Math.max(1, Math.floor(viewH / ROW_H) - 1);
      if (e.ctrlKey && e.key === ",") { e.preventDefault(); setSetupOpen(false); setSettingsOpen(true); refreshT1(); return; }
      switch (e.key) {
        case "ArrowDown": e.preventDefault(); move(sel + 1); break;
        case "ArrowUp": e.preventDefault(); move(sel - 1); break;
        case "PageDown": e.preventDefault(); move(sel + page); break;
        case "PageUp": e.preventDefault(); move(sel - page); break;
        case "Home": if (e.ctrlKey) { e.preventDefault(); move(0); } break;
        case "End": if (e.ctrlKey) { e.preventDefault(); move(selCount - 1); } break;
        case "Enter":
          if (setupOpenRef.current) {
            if ((e.target as HTMLElement | null)?.closest?.("button")) break; // native click
            e.preventDefault(); setupPrimaryRef.current(); break;
          }
          if (showPanelRef.current) {
            if ((e.target as HTMLElement | null)?.closest?.("button")) break; // native click
            e.preventDefault(); primaryRef.current(); break;
          }
          e.preventDefault(); void act(e.ctrlKey ? "reveal" : "open", current); break;
        case "Escape":
          e.preventDefault();
          if (setupOpenRef.current) { setSetupOpen(false); setSettingsOpen(false); refocus(); } else void backend.hide();
          break;
        case "Tab": {
          if (showPanelRef.current) break; // let focus move into the panel buttons
          e.preventDefault();
          const i = FILTERS.findIndex((f) => f.id === filter);
          setFilter(FILTERS[(i + (e.shiftKey ? FILTERS.length - 1 : 1)) % FILTERS.length].id);
          break;
        }
        case "c": case "C":
          if (e.ctrlKey && e.shiftKey) { e.preventDefault(); void act("copy", current); }
          else if (e.ctrlKey && !hasSelection && current) {
            e.preventDefault();
            backend.copyFiles([current.path]).then(() => flash("File copied")).catch(fail);
          }
          break;
        case "F2":
          e.preventDefault();
          if (current && sel < data.total) setRenaming(true);
          break;
        case "Delete":
          if (atEnd && current && sel < data.total) { e.preventDefault(); void doRecycle(sel); }
          break;
        case "z": case "Z":
          if (e.ctrlKey && !e.shiftKey && undoRef.current && (!inInput || inp!.value === "" || inp!.value === undoQuery.current)) {
            e.preventDefault();
            void doUndo();
          }
          break;
        case "ContextMenu":
          e.preventDefault();
          menuAtSelection();
          break;
        case "F10":
          if (e.shiftKey) { e.preventDefault(); menuAtSelection(); }
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [move, sel, viewH, act, current, filter, data.total, selCount, backend, renaming, flash, fail, doRecycle, doUndo, openMenu, openSemMenu, refreshT1, refocus]);

  useEffect(() => {
    const noMenu = (e: Event) => e.preventDefault();
    window.addEventListener("contextmenu", noMenu);
    return () => window.removeEventListener("contextmenu", noMenu);
  }, []);

  // moving the selection / reloading the list cancels an inline rename
  useEffect(() => { setRenaming(false); }, [sel, epoch, query, filter, sort]);

  // ---- first run -----------------------------------------------------------
  const demo = status.length > 0 && status.every((v) => v.state === "demo");
  const needsSetup = demo && (service === "not_installed" || service === "stopped") && !dismissed;
  const panelOpen = setupOpen || settingsOpen;
  const showPanel = panelOpen || (query.trim() === "" && (counting || needsSetup));
  showPanelRef.current = panelOpen || (needsSetup && query.trim() === "");
  setupOpenRef.current = panelOpen;
  summaryRef.current = summary;
  const runSetup = useCallback(async () => {
    if (setupBusy) return;
    setSetupBusy(true);
    setSetupErr(null);
    try {
      await (service === "stopped" ? backend.startService() : backend.installService());
      startCounting();
    } catch (e) {
      setSetupErr(String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, ""));
    } finally {
      setSetupBusy(false);
      refocus();
    }
  }, [backend, service, setupBusy, startCounting, refocus]);
  primaryRef.current = runSetup;
  useEffect(() => { if (query.trim() && counting && summary) setCounting(false); }, [query, counting, summary]);
  useEffect(() => { if (query.trim()) { setSetupOpen(false); setSettingsOpen(false); } }, [query]); // typing leaves the panels (a download keeps going)
  const openSetup = useCallback(() => {
    setSettingsOpen(false);
    setSetupOpen(true);
    models.refreshPlan();
    refreshT1();
  }, [models, refreshT1]);
  const closeSetup = useCallback(() => { setSetupOpen(false); refocus(); }, [refocus]);
  const openSettings = useCallback(() => { setSetupOpen(false); setSettingsOpen(true); refreshT1(); }, [refreshT1]);
  const closeSettings = useCallback(() => { setSettingsOpen(false); refocus(); }, [refocus]);
  const setFolders = useCallback((include: string[]) => {
    backend.tier1SetScopes(include, t1?.exclude ?? []).then(refreshT1).catch(fail);
  }, [backend, t1, refreshT1, fail]);
  const pauseIndexing = useCallback((p: boolean) => { backend.tier1Pause(p).then(refreshT1).catch(fail); }, [backend, refreshT1, fail]);

  // ---- render --------------------------------------------------------------
  const totalFiles = status.reduce((a, v) => a + v.files + v.folders, 0);
  const state = status.find((v) => v.state !== "live")?.state ?? (status.length ? "live" : "connecting");
  const statusText =
    state === "demo" ? `Demo data · ${service === "not_installed" ? "name service not installed" : service === "stopped" ? "name service stopped" : "searchd not running"}`
    : state === "offline" ? "searchd offline — reconnecting…"
    : totalFiles ? `${totalFiles.toLocaleString("en-US")} files · ${state}`
    : state === "scanning" ? "Indexing…"
    : "Connecting…";
  const heading = data.recent
    ? "Recent"
    : query.trim() === ""
      ? `Recent · ${FILTERS.find((f) => f.id === filter)?.label}`
      : null;
  const sortLabel = SORTS.find((s) => s.id === sort)!.label;
  const visible = useMemo(() => {
    const out: React.ReactNode[] = [];
    for (let i = first; i <= last; i++) {
      const h = data.rows[i];
      const thumbKind = h ? ["image", "video"].includes(kindOf(h.name, h.is_dir)) : false;
      out.push(
        <Row key={i} hit={h} query={query} selected={i === sel} top={i * ROW_H}
             icon={h ? backend.iconUrl(h.path, h.is_dir, thumbKind) : null} thumb={thumbKind}
             renaming={renaming && i === sel} both={!!h && semKeys.has(pathKey(h.path))}
             onPick={() => {
               if (suppressClick.current) { suppressClick.current = false; return; }
               setSel(i); void act("open", h);
             }}
             onHover={() => setSel((s) => (s === i ? s : i))}
             onDown={(e) => { if (h) onRowDown(e, h); }}
             onMenu={(e) => { e.preventDefault(); if (!h) return; setSel(i); void openMenu(i, e.clientX, e.clientY); }}
             onRenameCommit={(n) => void doRename(i, n)}
             onRenameCancel={() => { setRenaming(false); refocus(); }} />,
      );
    }
    return out;
  }, [first, last, data.rows, query, sel, act, backend, renaming, semKeys, onRowDown, openMenu, doRename, refocus]);

  return (
    <div className="shell">
      <header className="searchbar" data-tauri-drag-region>
        <svg className="glass" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
          <circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" />
        </svg>
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search files and folders"
          spellCheck={false}
          autoComplete="off"
          autoFocus
          aria-label="Search"
        />
        {query && (
          <button className="clear" onClick={() => { setQuery(""); inputRef.current?.focus(); }} aria-label="Clear" tabIndex={-1}>
            <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
          </button>
        )}
        <kbd className="hotkey">Alt Space</kbd>
      </header>

      <div className="chips" role="tablist">
        {FILTERS.map((f) => (
          <button key={f.id} role="tab" aria-selected={filter === f.id} className={"chip" + (filter === f.id ? " on" : "")}
                  tabIndex={-1} onClick={() => { setFilter(f.id); inputRef.current?.focus(); }}>
            {f.label}
          </button>
        ))}
        <span className="spacer" />
        <button className="sort" tabIndex={-1} title="Change sort order"
                onClick={() => { setSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + 1) % SORTS.length].id); inputRef.current?.focus(); }}>
          {sortLabel}
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
        </button>
        <button className="gear" tabIndex={-1} title="Settings (Ctrl+,)" aria-label="Settings" onClick={() => (settingsOpen ? closeSettings() : openSettings())}>
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1.08-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1.08 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.85 1 1.51 1H21a2 2 0 1 1 0 4h-.09c-.66 0-1.25.4-1.51 1z" />
          </svg>
        </button>
      </div>

      {showPanel && (settingsOpen ? (
        <Settings backend={backend} t1={t1} onFolders={setFolders} onPause={pauseIndexing} onSetup={openSetup} onClose={closeSettings} onError={fail} />
      ) : setupOpen ? (
        <Setup t1={t1} models={models} onFolders={setFolders} onPause={pauseIndexing} onClose={closeSetup} primaryRef={setupPrimaryRef} />
      ) : (
        <FirstRun
          mode={counting ? "counter" : "setup"} service={service} busy={setupBusy} error={setupErr}
          rows={status.filter((v) => v.state !== "demo")} summary={summary}
          onPrimary={() => void runSetup()}
          onDismiss={() => { setDismissed(true); setSetupErr(null); refocus(); }}
          onSemantic={t1?.state === "models_missing" ? openSetup : undefined}
        />
      ))}

      {!showPanel && heading && <div className="section">{heading}</div>}

      <div className="list" hidden={showPanel} ref={listRef} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)} role="listbox">
        <div className="spacer-v" style={{ height: data.total * ROW_H }}>{visible}</div>
        {loaded && data.total === 0 && (
          <div className="empty">
            <svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21M8 10.5h5" /></svg>
            <strong>No {semRows.length ? "name matches" : "results"}{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</strong>
            <span>Check the spelling, or try a different filter.</span>
          </div>
        )}
      </div>

      {!showPanel && semRows.length > 0 && (
        <div className="sem" ref={semRef}>
          <div className="section">By meaning</div>
          <div className="sem-rows" role="listbox" aria-label="Results by meaning">
            {semRows.map((h, i) => (
              <SemRow key={h.path} hit={h} query={sem.query} selected={data.total + i === sel}
                      icon={backend.iconUrl(h.path, false, true)} thumb
                      onPick={() => { setSel(data.total + i); void act("open", { name: baseName(h.path), path: h.path, is_dir: false, size: null, mtime: null }); }}
                      onHover={() => setSel((s) => (s === data.total + i ? s : data.total + i))}
                      onMenu={(e) => { e.preventDefault(); setSel(data.total + i); void openSemMenu(h.path, e.clientX, e.clientY); }} />
            ))}
          </div>
        </div>
      )}

      {!showPanel && t1Off && !hintOff && query.trim() !== "" && (
        <SemanticHint onSetup={openSetup} onDismiss={() => setHintOff(true)} />
      )}

      <footer className="footer">
        <span className={"status s-" + state}><i className="dot" />{statusText}</span>
        {toast ? (
          <span className="toast" role="status">
            {toast.msg}
            {toast.undo && <> · <button className="undo" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => void doUndo()}>Undo</button></>}
          </span>
        ) : (
          !data.recent && loaded && <span className="count">{data.total.toLocaleString("en-US")} result{data.total === 1 ? "" : "s"}{data.elapsedMs !== null && ` · ${data.elapsedMs < 1 ? "<1" : Math.round(data.elapsedMs)} ms`}</span>
        )}
        <span className="spacer" />
        <span className="hints">
          <span><kbd>↵</kbd> Open</span>
          <span><kbd>Ctrl ↵</kbd> Reveal</span>
          <span><kbd>Ctrl ⇧ C</kbd> Path</span>
          <span><kbd>F2</kbd> Rename</span>
          <span><kbd>Del</kbd> Recycle</span>
        </span>
      </footer>
    </div>
  );
}
