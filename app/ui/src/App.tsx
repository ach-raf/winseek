import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Filter, Hit, HitBox, LaunchCfg, LaunchHit, ServiceState, Sort, VolumeStatus } from "./types";
import { backspaceDropsChip, hasRanges, matchesRanges, nameQuery, parseQuery, type Parsed } from "./filters";
import { calc } from "./calc.ts";
import { FirstRun, type Summary } from "./FirstRun";
import { Setup } from "./Setup";
import { Settings, type SettingsSection } from "./Settings";
import { Preview } from "./Preview";
import { SemanticHint, pathKey, semAccepts, whyName } from "./Semantic";
import { Highlight, Overview, RenameInput, type ItemHandlers } from "./Overview";
import { buildOverview, expandable, ovMove, type Expand, type OvItem } from "./overviewLogic";
import { portableApps, launchForFilter } from "./launchLogic";
import * as qh from "./queryHistory";
import { combos, matches as matchesKey } from "./keymap";
import { footerHints, HintChip, Shortcuts, tooltip, type RowKind } from "./Shortcuts";
import { useModels, useSemantic, useTier1 } from "./hooks";
import { applyAccent } from "./accent";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize, kindOf, splitDir } from "./format";

const ROW_H = 54;
const PAGE = 80;
const OVERSCAN = 6;

const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "apps", label: "Apps" },
  { id: "settings", label: "Settings" }, // launcher-only scope: no name-index rows
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

const SCAN_PAGE = 2000;
const SCAN_MAX = 30000;

/**
 * Date / size filters are not part of the Tier 0 query language yet, so they are applied here: pull name
 * results in pages (newest first for a date filter, biggest first for a size filter, so the scan can stop as
 * soon as it is past the range) and keep what fits. The result is fully loaded; no paging afterwards.
 */
async function scanRanges(
  backend: Backend, query: string, filter: Filter, sort: Sort, p: Parsed, cancelled: () => boolean,
): Promise<{ rows: Hit[]; partial: boolean; ms: number }> {
  const t0 = performance.now();
  const byDate = !!p.date && (sort === "rel" || sort === "date");
  const bySize = !byDate && !!p.size && (sort === "rel" || sort === "size");
  const srvSort: Sort = byDate ? "date" : bySize ? "size" : sort;
  const rows: Hit[] = [];
  let offset = 0, partial = false;
  for (;;) {
    const r = await backend.search({ query, offset, limit: SCAN_PAGE, sort: srvSort, filter });
    if (cancelled()) break;
    let stop = false;
    for (const h of r.hits) {
      if (byDate && (h.mtime ?? 0) < p.date!.from) { stop = true; break; }
      if (bySize && p.size!.min !== null && h.size !== null && h.size < p.size!.min) { stop = true; break; }
      if (matchesRanges(p, h)) rows.push(h);
    }
    offset += r.hits.length;
    if (stop || r.hits.length === 0 || offset >= r.total) break;
    if (offset >= SCAN_MAX) { partial = true; break; }
  }
  return { rows, partial, ms: performance.now() - t0 };
}

const Row = memo(function Row({
  hit, query, why, selected, top, icon, thumb, renaming, onPick, onOpen, onDown, onMenu, onRenameCommit, onRenameCancel,
}: {
  hit: Hit | undefined; query: string; why: string; selected: boolean; top: number; icon: string | null; thumb: boolean; renaming: boolean;
  onPick: () => void; onOpen: () => void; onDown: (e: React.MouseEvent) => void; onMenu: (e: React.MouseEvent) => void;
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
      data-sel={selected ? "1" : undefined}
      onClick={renaming ? undefined : onPick}
      onDoubleClick={renaming ? undefined : onOpen}
      onMouseDown={onDown}
      onContextMenu={onMenu}
      title={hit.path}
    >
      <FileIcon name={hit.name} isDir={hit.is_dir} src={icon} thumb={thumb} />
      <div className="meta">
        <div className="name">
          {renaming
            ? <RenameInput initial={hit.name} isDir={hit.is_dir} onCommit={onRenameCommit} onCancel={onRenameCancel} />
            : <><Highlight text={hit.name} query={query} /></>}
        </div>
        <div className="path">
          <span className="head">{head}</span><span className="tail">{tail}</span>
          {why && <span className="why-line" title="Why it matched">{why}</span>}
        </div>
      </div>
      <div className="side">
        <span className="size">{hit.is_dir ? "Folder" : formatSize(hit.size)}</span>
        <span className="date">{formatDate(hit.mtime)}</span>
      </div>
    </div>
  );
});

type Undo = { kind: "rename"; newPath: string; oldName: string } | { kind: "recycle"; paths: string[] } | { kind: "unhide"; id: string };

interface Data {
  total: number; rows: (Hit | undefined)[]; elapsedMs: number | null; recent: boolean;
  /** Every row is loaded (a date / size filter was applied here); `partial` = the scan hit its cap. */
  full: boolean; partial: boolean;
  /** Bumped when pages are filled into `rows` in place (a copy per page would be O(rows loaded)). */
  ver: number;
}

export default function App({ backend }: { backend: Backend }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("rel");
  const [sel, setSel] = useState(0);
  const [data, setData] = useState<Data>({ total: 0, rows: [], elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
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
  // The preview pane always shows the selection (Spotlight); Space (`pvOpen`) blows it up to a full-window
  // Quick Look. `pv` is the (debounced) hit it shows. `navigated`: the user has moved through the results with the
  // keyboard since the last keystroke in the box, so Space means "Quick Look", not a space.
  const [pvOpen, setPvOpen] = useState(false);
  const [pv, setPv] = useState<{ hit: Hit; snippet: string | null; page: number | null; boxes: HitBox[] | null } | null>(null);
  const navigated = useRef(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("meaning");
  const [cheat, setCheat] = useState(false); // the F1 / Ctrl+/ shortcut cheat sheet
  const [hintOff, setHintOff] = useState(false);
  const [summary, setSummary] = useState<Summary | null>(null);
  // Launcher (apps / settings / commands): hits per keystroke, the armed confirm row, the
  // in-app context menu, and the launcher config (the portable-exes toggle lives in it).
  const [launch, setLaunch] = useState<LaunchHit[]>([]);
  const [launchVer, setLaunchVer] = useState(0);
  const [lcfg, setLcfg] = useState<LaunchCfg | null>(null);
  const [armed, setArmed] = useState<string | null>(null);
  const [launchMenu, setLaunchMenu] = useState<{ hit: LaunchHit; x: number; y: number } | null>(null);
  const t0 = useRef<number | null>(null);
  const sawScan = useRef(false);
  const showPanelRef = useRef(false);
  const summaryRef = useRef<Summary | null>(null);
  const primaryRef = useRef<() => void>(() => {});
  const setupOpenRef = useRef(false);
  const setupPrimaryRef = useRef<() => void>(() => {});
  const ovRef = useRef<HTMLDivElement>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  // Search history (Alt+Up / Alt+Down): loaded once, appended when the user acts on a result.
  const hist = useRef<string[]>(qh.load());
  const histAt = useRef(-1); // -1 = the live input
  const recordQuery = useCallback(() => {
    const q = inputRef.current?.value ?? "";
    if (!q.trim()) return;
    hist.current = qh.push(hist.current, q);
    histAt.current = -1;
    qh.save(hist.current);
  }, []);
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

  // ---- natural filters -----------------------------------------------------
  // "photos from last summer" -> chips + free text. A removed chip's key goes into `off`; its words then count as text.
  const [off, setOff] = useState<ReadonlySet<string>>(new Set());
  const parsed = useMemo(() => parseQuery(query, { off }), [query, off]);
  useEffect(() => { if (!query.trim()) setOff((o) => (o.size ? new Set() : o)); }, [query]);
  const nameQ = nameQuery(parsed);
  // "settings" is a launcher-only scope, not a name-index filter — via the word in the query or the tab.
  const effFilter: Filter = parsed.kind && parsed.kind !== "settings" ? parsed.kind : filter;
  const settingsScope = parsed.kind === "settings" || filter === "settings";
  const rangeKey = JSON.stringify([parsed.date, parsed.size]);
  const chipLabels = useMemo(() => parsed.chips.map((c) => c.label), [parsed]);
  const dropChip = useCallback((key: string) => {
    setOff((o) => new Set(o).add(key));
    inputRef.current?.focus();
  }, []);

  // ---- search by meaning ---------------------------------------------------
  const [t1, refreshT1] = useTier1(backend, setupOpen || settingsOpen, epoch);
  const models = useModels(backend, refreshT1);
  const t1Off = !!t1 && (t1.state === "models_missing" || t1.state === "unavailable");
  // Text found inside files (documents, PDFs, screenshot OCR) is searchable before the models exist, so the
  // meaning search runs whenever Tier 1 is up; without models it answers with text hits only.
  const sem = useSemantic(backend, parsed.text, !!t1 && t1.state !== "unavailable" && !isRecent, effFilter, epoch, hasRanges(parsed) || !!parsed.ext);
  // Meaning hits obey the same chips as the name results (kind / extension always; date / size when the hit carries them).
  const semHits = useMemo(() => sem.hits.filter((h) => semAccepts(h, parsed)), [sem.hits, parsed]);
  const semMap = useMemo(() => new Map(semHits.map((h) => [pathKey(h.path), h])), [semHits]);
  const selCount = data.total;

  // ---- Spotlight overview --------------------------------------------------
  // A typed search shows sections (Top Hit, Files, Images, Documents) with "Show all"; the recent list, a sort
  // other than best match, and "Show all files" use the full virtualised list instead.
  const [expand, setExpand] = useState<Expand>(null);
  useEffect(() => { setExpand(null); }, [query, filter]);
  const overview = !isRecent && sort === "rel" && expand !== "files";
  // The calculator Top Hit belongs to "all" only — a type filter means "show me that kind of file".
  const calcResult = useMemo(() => (settingsScope || expand || effFilter !== "all" ? null : calc(parsed.text)), [parsed.text, settingsScope, expand, effFilter]);
  // Launcher rows follow the type filter (a Folders search must not put a settings page in the Top Hit slot).
  const launchSel = useMemo(
    () => launchForFilter(launch, settingsScope ? "settings" : effFilter),
    [launch, settingsScope, effFilter], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const portable = useMemo(
    () => (lcfg?.portable === false ? [] : portableApps(data.rows.filter((h): h is Hit => !!h), parsed.text, launch)),
    [lcfg, data.rows, data.ver, parsed.text, launch], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const ov = useMemo(
    () => buildOverview({
      names: settingsScope ? [] : data.rows, nameTotal: settingsScope ? 0 : data.total, sem: semHits, text: parsed.text,
      expand: settingsScope ? "settings" : expand, isImage: (n) => kindOf(n, false) === "image", launch: launchSel, portable, calc: calcResult ?? undefined,
    }),
    [data.rows, data.ver, data.total, semHits, parsed.text, expand, launchSel, portable, calcResult, settingsScope], // eslint-disable-line react-hooks/exhaustive-deps
  );
  // The overview selection is kept by item key so results arriving later (meaning hits) do not move it.
  const [oselKey, setOselKey] = useState<string | null>(null);
  useEffect(() => { setOselKey(null); }, [query, filter, expand]);
  const osel = Math.max(0, oselKey ? ov.flat.findIndex((x) => x.key === oselKey) : 0);
  const ovItem: OvItem | undefined = overview ? ov.flat[osel] : undefined;

  // ---- data loading --------------------------------------------------------
  useEffect(() => {
    const my = ++seq.current;
    inflight.current.clear();
    (async () => {
      if (isRecent) {
        const hits = await backend.recent(40);
        if (my !== seq.current) return;
        rowsRef.current = hits;
        setData({ total: hits.length, rows: hits, elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
      } else if (hasRanges(parsed)) {
        const r = await scanRanges(backend, nameQ, effFilter, sort, parsed, () => my !== seq.current);
        if (my !== seq.current) return;
        rowsRef.current = r.rows;
        setData({ total: r.rows.length, rows: r.rows, elapsedMs: r.ms, recent: false, full: true, partial: r.partial, ver: 0 });
      } else {
        const r = await backend.search({ query: nameQ, offset: 0, limit: PAGE, sort, filter: effFilter });
        if (my !== seq.current) return;
        // Only the loaded rows are stored (`data.total` is the length); a one-letter query can match
        // millions of names and must not allocate a slot for each on every keystroke.
        const rows: (Hit | undefined)[] = r.hits.slice();
        rowsRef.current = rows;
        setData({ total: r.total, rows, elapsedMs: r.elapsed_us / 1000, recent: false, full: false, partial: false, ver: 0 });
      }
      setLoaded(true);
      setSel(0);
      setScrollTop(0);
      if (listRef.current) listRef.current.scrollTop = 0;
    })().catch(console.error);
  }, [backend, nameQ, effFilter, rangeKey, sort, isRecent, epoch]); // eslint-disable-line react-hooks/exhaustive-deps

  const ensurePages = useCallback((first: number, last: number) => {
    if (data.recent || data.full) return;
    const my = seq.current;
    for (let p = Math.floor(first / PAGE); p <= Math.floor(last / PAGE); p++) {
      if (rowsRef.current[p * PAGE] || inflight.current.has(p) || p * PAGE >= data.total) continue;
      inflight.current.add(p);
      backend.search({ query: nameQ, offset: p * PAGE, limit: PAGE, sort, filter: effFilter }).then((r) => {
        if (my !== seq.current) return;
        const rows = rowsRef.current;
        r.hits.forEach((h, i) => (rows[p * PAGE + i] = h));
        setData((d) => ({ ...d, ver: d.ver + 1 }));
      }).catch(console.error);
    }
  }, [backend, data.recent, data.full, data.total, nameQ, sort, effFilter]);

  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(data.total - 1, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);
  useEffect(() => { if (last >= first) ensurePages(first, last); }, [first, last, ensurePages]);

  // ---- status / window events ---------------------------------------------
  const fast = counting && !summary; // 250 ms while the first scan is being watched
  const refreshStatus = useCallback(() => {
    backend.status().then((st) => {
      // Polled every second: an unchanged answer must not re-render the whole popup.
      setStatus((prev) => (JSON.stringify(prev) === JSON.stringify(st) ? prev : st));
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

  // ---- launcher data --------------------------------------------------------
  const launchSeq = useRef(0);
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    backend.launchGetConfig().then((c) => { if (!dead) setLcfg(c); }).catch(console.error);
    backend.onLaunchCatalogChanged(() => {
      setLaunchVer((v) => v + 1);
      backend.launchGetConfig().then(setLcfg).catch(() => {});
    }).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend]);
  useEffect(() => {
    const text = parsed.text;
    if (!text.trim()) { setLaunch([]); return; }
    const my = ++launchSeq.current;
    backend.launchSearch(text, 30).then((hits) => { if (my === launchSeq.current) setLaunch(hits); }).catch(console.error);
  }, [backend, parsed.text, launchVer]); // eslint-disable-line react-hooks/exhaustive-deps

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
  const openLibrary = useCallback(() => { backend.openLibrary().then(() => backend.hide()).catch(fail); }, [backend, fail]);
  /** Settings opened straight on the Status section (tray item, Ctrl+Shift+L). */
  const openStatus = useCallback(() => { setSetupOpen(false); setSettingsOpen(true); setSettingsSection("status"); refreshT1(); }, [refreshT1]);

  const semSel = ovItem?.sem;
  const current: Hit | undefined = overview ? ovItem?.hit : data.rows[sel];
  /** Index of the selection in the name results, when it is one (rename / recycle / menu work on those rows). */
  const curIdx: number | undefined = overview ? ovItem?.nameIdx : current ? sel : undefined;

  const curSnippet = semSel?.snippet ?? (current ? semMap.get(pathKey(current.path))?.snippet : null) ?? null;
  const curPage = semSel?.page ?? (current ? semMap.get(pathKey(current.path))?.page : null) ?? null;
  const curBoxes = semSel?.boxes ?? (current ? semMap.get(pathKey(current.path))?.boxes : null) ?? null;
  const curKey = current ? `${current.path}|${curSnippet ?? ""}|${curPage ?? ""}|${curBoxes?.length ?? 0}` : "";
  const currentRef = useRef(current);
  currentRef.current = current;
  const snippetRef = useRef(curSnippet);
  snippetRef.current = curSnippet;
  const pageRef = useRef(curPage);
  pageRef.current = curPage;
  const boxesRef = useRef(curBoxes);
  boxesRef.current = curBoxes;
  // The pane follows the selection ~120 ms after it settles (arrowing through a list must not load every file).
  // Launch rows (apps / settings / commands) have no preview: the pane stays empty for them.
  const isLaunchSel = !!(overview && ovItem?.launch);
  const pvShown = useRef(false);
  useEffect(() => {
    const c = currentRef.current;
    if (!c || isLaunchSel) { pvShown.current = false; setPv(null); return; }
    const set = () => { pvShown.current = true; setPv({ hit: c, snippet: snippetRef.current, page: pageRef.current, boxes: boxesRef.current }); };
    if (!pvShown.current) { set(); return; }
    const t = window.setTimeout(set, 120);
    return () => window.clearTimeout(t);
  }, [curKey, isLaunchSel]);
  useEffect(() => { navigated.current = false; }, [query]);

  const act = useCallback(async (kind: "open" | "reveal" | "copy", h: Hit | undefined) => {
    if (!h) return;
    recordQuery();
    try {
      if (kind === "copy") { await backend.copyPath(h.path); flash("Path copied"); return; }
      if (kind === "open") await backend.openPath(h.path); else await backend.revealPath(h.path);
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, flash, fail, recordQuery]);

  const armTimer = useRef<number | undefined>(undefined);
  const armConfirm = useCallback((id: string) => {
    window.clearTimeout(armTimer.current);
    setArmed(id);
    armTimer.current = window.setTimeout(() => setArmed(null), 4000);
  }, []);
  useEffect(() => { setArmed(null); }, [oselKey, query]); // selection / query change disarms

  const launchAct = useCallback(async (h: LaunchHit, mode: "open" | "reveal" | "copy" | "admin") => {
    const id = h.item.id;
    const q = parsed.text;
    recordQuery();
    try {
      // The winseek panels are handled here; the backend only records the pick.
      if (id === "cmd:winseek_settings") {
        void backend.launchRecord(id, q);
        setSetupOpen(false);
        setSettingsOpen(true);
        refreshT1();
        await backend.hide();
        return;
      }
      if (id === "cmd:winseek_library") { void backend.launchRecord(id, q); openLibrary(); return; }
      if (mode === "copy") { await backend.launchCopy(id); flash("Target copied"); return; }
      if (mode === "reveal") { await backend.launchReveal(id); await backend.hide(); return; }
      if (h.confirm && armed !== id) { armConfirm(id); return; } // second Enter confirms
      await backend.launchRun(id, q, mode === "admin");
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, parsed.text, armed, armConfirm, flash, fail, openLibrary, refreshT1, recordQuery]);

  const removeRow = useCallback((i: number) => {
    const rows = rowsRef.current.slice();
    rows.splice(i, 1);
    rowsRef.current = rows;
    setData((d) => ({ ...d, rows, total: Math.max(0, d.total - 1) }));
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
      } else if (u.kind === "unhide") {
        await backend.launchUnhide(u.id);
        setLaunchVer((v) => v + 1);
        flash("Back in the results");
      } else {
        const n = await backend.restoreRecycled(u.paths);
        flash(n > 0 ? "Restored" : "Nothing to restore");
      }
      setEpoch((e) => e + 1); // re-run the search to refresh the list
    } catch (e) { fail(e); }
  }, [backend, flash, fail]);

  const hideLaunch = useCallback((h: LaunchHit) => {
    setLaunchMenu(null);
    void backend.launchHide(h.item.id).then(() => {
      setLaunch((prev) => prev.filter((x) => x.item.id !== h.item.id));
      pushUndo({ kind: "unhide", id: h.item.id }, "Hidden from results");
    }).catch(fail);
  }, [backend, pushUndo, fail]);

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
    setSel((s) => (s >= selCount ? Math.max(0, selCount - 1) : s));
  }, [sel, selCount]);
  useEffect(() => {
    if (!overview) return;
    const el = ovRef.current?.querySelector('[data-sel="1"]');
    if (el) el.scrollIntoView({ block: "nearest" });
    else if (osel === 0) ovRef.current?.scrollTo({ top: 0 });
  }, [overview, osel, oselKey]);
  const ovGo = useCallback((i: number) => { const it = ov.flat[i]; if (it) setOselKey(it.key); }, [ov.flat]);

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
        if (overview) {
          const sr = ovRef.current?.querySelector('[data-sel="1"]')?.getBoundingClientRect();
          if (!sr) return;
          const x = sr.left + Math.min(120, sr.width / 2), y = sr.top + sr.height / 2;
          if (ovItem?.launch) { setLaunchMenu({ hit: ovItem.launch, x, y }); return; }
          if (curIdx !== undefined) void openMenu(curIdx, x, y); else void openSemMenu(current.path, x, y);
          return;
        }
        const r = el.getBoundingClientRect();
        void openMenu(sel, r.left + 120, r.top + 4 + sel * ROW_H - el.scrollTop + ROW_H / 2);
      };
      const ovStep = (dir: Parameters<typeof ovMove>[2]) => { e.preventDefault(); navigated.current = true; ovGo(ovMove(ov.flat, osel, dir)); };
      // The cheat sheet is its own layer: F1 / Ctrl+/ toggles it, Esc closes it first of all.
      if (combos("popup.cheatsheet").some((c) => matchesKey(e, c))) { e.preventDefault(); setCheat((v) => !v); return; }
      // Alt combos (physical-key matching, so Alt+1 works on AZERTY) must preventDefault:
      // WebView2 otherwise beeps or moves focus to the menu bar.
      if (e.altKey && !e.ctrlKey && !e.metaKey) {
        const fi = FILTERS.findIndex((_, i) => matchesKey(e, `Alt+${i + 1}`));
        if (fi >= 0) {
          e.preventDefault();
          const k = parsed.chips.find((c) => c.type === "kind");
          if (k) dropChip(k.key); // a filter key overrides the kind found in the text (like the tab click)
          setFilter(FILTERS[fi].id);
          inputRef.current?.focus();
          return;
        }
        if (matchesKey(e, "Alt+S")) {
          e.preventDefault();
          setSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + (e.shiftKey ? SORTS.length - 1 : 1)) % SORTS.length].id);
          inputRef.current?.focus();
          return;
        }
        if (matchesKey(e, "Alt+Right")) { e.preventDefault(); const ex = expandable(ovItem); if (ex) setExpand(ex); return; }
        if (matchesKey(e, "Alt+Left")) { e.preventDefault(); setExpand(null); return; }
        if (matchesKey(e, "Alt+Up") || matchesKey(e, "Alt+Down")) {
          e.preventDefault();
          const dir: -1 | 1 = matchesKey(e, "Alt+Up") ? -1 : 1;
          const r = qh.step(hist.current, histAt.current, inp?.value ?? "", dir);
          histAt.current = r.at;
          setQuery(r.q);
          inputRef.current?.focus();
          return;
        }
        if (matchesKey(e, "Alt+Enter")) {
          e.preventDefault();
          // Launch rows have nothing to show properties of (like Space today).
          if (!ovItem?.launch && current) backend.showProperties(current.path).catch(fail);
          return;
        }
      }
      if (matchesKey(e, "Ctrl+Shift+L")) { e.preventDefault(); openStatus(); return; }
      if (overview && !e.altKey && !e.shiftKey) {
        // Ctrl+arrows / PageUp / PageDown jump between sections; left / right walk the image grid once the
        // keyboard is in the results (before that they move the caret in the search box).
        if (e.key === "ArrowDown") return ovStep(e.ctrlKey ? "next" : "down");
        if (e.key === "ArrowUp") return ovStep(e.ctrlKey ? "prev" : "up");
        if (e.key === "PageDown") return ovStep("next");
        if (e.key === "PageUp") return ovStep("prev");
        if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && navigated.current && ovItem?.section === "images") {
          return ovStep(e.key === "ArrowLeft" ? "left" : "right");
        }
      }
      const page = Math.max(1, Math.floor(viewH / ROW_H) - 1);
      if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "l") { e.preventDefault(); openLibrary(); return; }
        if (e.ctrlKey && e.key === ",") { e.preventDefault(); setSetupOpen(false); setSettingsOpen(true); refreshT1(); return; }
      switch (e.key) {
        case "ArrowDown": e.preventDefault(); navigated.current = true; move(sel + 1); break;
        case "ArrowUp": e.preventDefault(); navigated.current = true; move(sel - 1); break;
        case "PageDown": e.preventDefault(); navigated.current = true; move(sel + page); break;
        case "PageUp": e.preventDefault(); navigated.current = true; move(sel - page); break;
        case "Home": if (e.ctrlKey) { e.preventDefault(); navigated.current = true; move(0); } break;
        case "End": if (e.ctrlKey) { e.preventDefault(); navigated.current = true; move(selCount - 1); } break;
        case " ":
          // Space closes Quick Look; it opens it when it cannot be a typed space (empty box, or after arrowing
          // through the results). Launch rows have no preview: Space is a no-op there.
          if (ovItem?.launch) break;
          if (pvOpen) { e.preventDefault(); setPvOpen(false); }
          else if (current && !showPanelRef.current && !setupOpenRef.current && (e.ctrlKey || navigated.current || query.trim() === "")) {
            e.preventDefault(); setPvOpen(true); recordQuery();
          }
          break;
        case "Enter":
          if (setupOpenRef.current) {
            if ((e.target as HTMLElement | null)?.closest?.("button")) break; // native click
            e.preventDefault(); setupPrimaryRef.current(); break;
          }
          if (showPanelRef.current) {
            if ((e.target as HTMLElement | null)?.closest?.("button")) break; // native click
            e.preventDefault(); primaryRef.current(); break;
          }
          e.preventDefault();
          if (ovItem?.calc !== undefined) { // calculator Top Hit: Enter copies the result
            void backend.copyTextHtml(ovItem.calc).then(() => backend.hide()).catch(fail);
            break;
          }
          if (ovItem?.launch) { void launchAct(ovItem.launch, e.ctrlKey && e.shiftKey ? "admin" : e.ctrlKey ? "reveal" : "open"); break; }
          void act(e.ctrlKey ? "reveal" : "open", current);
          break;
        case "Escape":
          e.preventDefault();
          if (cheat) setCheat(false);
          else if (armed) setArmed(null);
          else if (launchMenu) setLaunchMenu(null);
          else if (pvOpen) setPvOpen(false);
          else if (expand && !setupOpenRef.current) setExpand(null);
          else if (setupOpenRef.current) { setSetupOpen(false); setSettingsOpen(false); refocus(); } else void backend.hide();
          break;
        case "Backspace": {
          // Remove the last filter chip only at caret 0 with no selection (same path as the chip's ×).
          const atStart = inInput && inp!.selectionStart === 0 && inp!.selectionEnd === 0;
          if (backspaceDropsChip(atStart, hasSelection, parsed.chips.length)) {
            e.preventDefault();
            dropChip(parsed.chips[parsed.chips.length - 1].key);
          }
          break;
        }
        case "Tab": {
          if (showPanelRef.current) break; // let focus move into the panel buttons
          e.preventDefault();
          const i = FILTERS.findIndex((f) => f.id === filter);
          setFilter(FILTERS[(i + (e.shiftKey ? FILTERS.length - 1 : 1)) % FILTERS.length].id);
          break;
        }
        case "c": case "C":
          if (e.ctrlKey && e.altKey && !e.shiftKey) {
            // Copy the file NAME only (Ctrl+Shift+C stays "copy full path"). Launch rows: no-op.
            e.preventDefault();
            if (current && !ovItem?.launch) {
              const dot = current.name.lastIndexOf(".");
              const base = current.is_dir || dot <= 0 ? current.name : current.name.slice(0, dot);
              backend.copyTextHtml(base).then(() => flash("Name copied")).catch(fail);
            }
          }
          else if (e.ctrlKey && e.shiftKey) { e.preventDefault(); if (ovItem?.launch) void launchAct(ovItem.launch, "copy"); else void act("copy", current); }
          else if (e.ctrlKey && !hasSelection && current) {
            e.preventDefault();
            backend.copyFiles([current.path]).then(() => flash("File copied")).catch(fail);
          }
          break;
        case "F2":
          e.preventDefault();
          if (current && curIdx !== undefined && !ovItem?.launch) setRenaming(true);
          break;
        case "Delete":
          if (atEnd && current && !ovItem?.launch) { // launch rows: nothing to delete
            e.preventDefault();
            if (curIdx !== undefined) void doRecycle(curIdx);
            else backend.recycle([current.path]).then(() => { pushUndo({ kind: "recycle", paths: [current.path] }, "Moved to Recycle Bin"); setEpoch((x) => x + 1); }).catch(fail);
          }
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
  }, [move, sel, viewH, act, current, curIdx, query, pvOpen, filter, selCount, backend, renaming, flash, fail, doRecycle, doUndo, openMenu, openSemMenu, refreshT1, refocus, openLibrary, overview, ov.flat, osel, ovItem, ovGo, expand, pushUndo, launchAct, armed, launchMenu, cheat, openStatus, parsed.chips, dropChip, sort, recordQuery]);

  useEffect(() => {
    const noMenu = (e: Event) => e.preventDefault();
    window.addEventListener("contextmenu", noMenu);
    return () => window.removeEventListener("contextmenu", noMenu);
  }, []);

  // moving the selection / reloading the list cancels an inline rename
  useEffect(() => { setRenaming(false); }, [sel, oselKey, epoch, query, filter, sort]);

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
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    backend.onOpenStatus(openStatus).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend, openStatus]);
  const setFolders = useCallback((include: string[]) => {
    backend.tier1SetScopes(include, t1?.exclude ?? []).then(refreshT1).catch(fail);
  }, [backend, t1, refreshT1, fail]);
  const removeFolder = useCallback((path: string) => {
    backend.tier1RemoveFolder(path).then(refreshT1).catch(fail);
  }, [backend, refreshT1, fail]);
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
        <Row key={i} hit={h} query={parsed.text} why={h && !data.recent ? whyName(h, nameQ, chipLabels, semMap.get(pathKey(h.path))?.score) : ""} selected={i === sel} top={i * ROW_H}
             icon={h ? backend.iconUrl(h.path, h.is_dir, thumbKind) : null} thumb={thumbKind}
             renaming={renaming && i === sel}
             onPick={() => {
               if (suppressClick.current) { suppressClick.current = false; return; }
               setSel(i); refocus();
             }}
             onOpen={() => { if (!suppressClick.current) void act("open", h); }}
             onDown={(e) => { if (h) onRowDown(e, h); }}
             onMenu={(e) => { e.preventDefault(); if (!h) return; setSel(i); void openMenu(i, e.clientX, e.clientY); }}
             onRenameCommit={(n) => void doRename(i, n)}
             onRenameCancel={() => { setRenaming(false); refocus(); }} />,
      );
    }
    return out;
  }, [first, last, data.rows, data.ver, data.recent, parsed.text, nameQ, chipLabels, semMap, sel, act, backend, renaming, onRowDown, openMenu, doRename, refocus]);

  const ovHandlers: ItemHandlers = useMemo(() => ({
    onSelect: (i) => {
      if (suppressClick.current) { suppressClick.current = false; return; }
      ovGo(i); refocus();
    },
    onOpen: (i) => {
      if (suppressClick.current) return;
      const it = ov.flat[i];
      if (it?.launch) { void launchAct(it.launch, "open"); return; }
      void act("open", it?.hit);
    },
    onMenu: (i, e) => {
      e.preventDefault();
      const it = ov.flat[i];
      if (!it) return;
      ovGo(i);
      if (it.launch) { setLaunchMenu({ hit: it.launch, x: e.clientX, y: e.clientY }); return; }
      if (it.nameIdx !== undefined) void openMenu(it.nameIdx, e.clientX, e.clientY); else void openSemMenu(it.hit.path, e.clientX, e.clientY);
    },
    onDown: (i, e) => { const it = ov.flat[i]; if (it && !it.launch) onRowDown(e, it.hit); }, // launch rows do not drag
    onRenameCommit: (i, n) => { const idx = ov.flat[i]?.nameIdx; if (idx !== undefined) void doRename(idx, n); },
    onRenameCancel: () => { setRenaming(false); refocus(); },
  }), [ov.flat, ovGo, act, launchAct, openMenu, openSemMenu, onRowDown, doRename, refocus]);
  const showList = !overview;
  const rowKind: RowKind =
    pvOpen ? "preview"
    : ovItem?.calc !== undefined ? "calc"
    : ovItem?.launch ? (({ app: "app", setting: "setting", command: "command" } as const)[ovItem.launch.item.kind])
    : current?.is_dir ? "folder"
    : current ? "file" : "file";
  const waitingSem = overview && sem.query !== parsed.text.trim() && parsed.text.trim().length >= 2 && !!t1 && t1.state !== "unavailable";
  const pvPane = !showPanel && !!pv && !!current;

  return (
    <div className="shell">
      <header
        className={"searchbar" + (query ? "" : " idle")}
        onMouseDown={(e) => {
          // The bar is the window's drag handle; the input and buttons keep their own behavior.
          if (e.button !== 0 || (e.target as HTMLElement).closest("input, button")) return;
          e.preventDefault();
          backend.movePopup().catch(fail);
        }}
      >
        <svg className="glass" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
          <circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" />
        </svg>
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => { histAt.current = -1; setQuery(e.target.value); }}
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

      {parsed.chips.length > 0 && (
        <div className="fchips" aria-label="Filters understood from your search">
          {parsed.chips.map((c) => (
            <span key={c.key} className={"fchip " + c.type}>
              <span className="lbl">{c.label}</span>
              <button tabIndex={-1} aria-label={`Remove filter ${c.label}`} title="Remove this filter (its words are searched as text)"
                      onMouseDown={(e) => e.preventDefault()} onClick={() => dropChip(c.key)}>
                <svg viewBox="0 0 24 24" width="10" height="10" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
              </button>
            </span>
          ))}
        </div>
      )}

      <div className="chips">
        <div className="seg" role="tablist" aria-label="File type filter">
          {FILTERS.map((f) => (
            <button key={f.id} role="tab" aria-selected={effFilter === f.id} className={effFilter === f.id ? "on" : ""}
                    tabIndex={-1} onClick={() => {
                      const k = parsed.chips.find((c) => c.type === "kind");
                      if (k) dropChip(k.key); // a tab click overrides the kind found in the text
                      setFilter(f.id); inputRef.current?.focus();
                    }}>
              {f.label}
            </button>
          ))}
        </div>
        <span className="spacer" />
        <button className="sort" tabIndex={-1} title={tooltip("popup.sort")}
                onClick={() => { setSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + 1) % SORTS.length].id); inputRef.current?.focus(); }}>
          {sortLabel}
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
        </button>
        <button className="gear" tabIndex={-1} title={tooltip("popup.settings")} aria-label="Settings" onClick={() => (settingsOpen ? closeSettings() : openSettings())}>
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1.08-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1.08 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.85 1 1.51 1H21a2 2 0 1 1 0 4h-.09c-.66 0-1.25.4-1.51 1z" />
          </svg>
        </button>
      </div>

      {showPanel && (setupOpen ? (
        <Setup t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing} onClose={closeSetup} primaryRef={setupPrimaryRef} />
      ) : settingsOpen ? (
        <Settings backend={backend} t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing}
                  onSetup={openSetup} onClose={closeSettings} onLibrary={openLibrary}
                  section={settingsSection} onSection={setSettingsSection} onError={fail} />
      ) : (
        <FirstRun
          mode={counting ? "counter" : "setup"} service={service} busy={setupBusy} error={setupErr}
          rows={status.filter((v) => v.state !== "demo")} summary={summary}
          onPrimary={() => void runSetup()}
          onDismiss={() => { setDismissed(true); setSetupErr(null); refocus(); }}
          onSemantic={t1?.state === "models_missing" ? openSetup : undefined}
        />
      ))}

      <div className={"main" + (pvPane ? " split" : "") + (pvPane && pvOpen ? " ql" : "")} hidden={showPanel}>
      <div className="lcol">
      {!showPanel && showList && (heading || expand === "files") && (
        <div className="section">
          {expand === "files" ? (
            <button className="ov-back" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => setExpand(null)} aria-label="Back to all results">
              <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
              <span className="ov-title">Files <span className="ov-count">{data.total.toLocaleString("en-US")}</span></span>
            </button>
          ) : heading}
        </div>
      )}

      <div className="list" hidden={showPanel || !showList} ref={listRef} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)} role="listbox">
        <div className="spacer-v" style={{ height: data.total * ROW_H }}>{visible}</div>
        {loaded && data.total === 0 && (
          <div className="empty">
            <svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21M8 10.5h5" /></svg>
            <strong>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</strong>
            <span>Check the spelling, or try a different filter.</span>
          </div>
        )}
      </div>

      {!showPanel && overview && (ov.flat.length > 0 ? (
        <Overview sections={ov.sections} sel={osel} renaming={renaming} query={parsed.text} backend={backend} expanded={!!expand}
                  handlers={ovHandlers} onExpand={(id) => { if (id !== "top") setExpand(id as Expand); refocus(); }} onBack={() => { setExpand(null); refocus(); }} scrollRef={ovRef} armed={armed} />
      ) : loaded && !waitingSem ? (
        <div className="empty">
          <svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21M8 10.5h5" /></svg>
          <strong>No results for &ldquo;{query.trim()}&rdquo;</strong>
          <span>Nothing by name, and nothing that clearly matches by meaning.</span>
        </div>
      ) : <div className="ov" />)}

      {!showPanel && t1Off && !hintOff && query.trim() !== "" && (
        <SemanticHint onSetup={openSetup} onDismiss={() => setHintOff(true)} />
      )}
      </div>
      {pvPane && (
        <Preview backend={backend} hit={pv.hit} query={parsed.text} snippet={pv.snippet} page={pv.page} boxes={pv.boxes}
                 onOpen={() => void act("open", pv.hit)} onReveal={() => void act("reveal", pv.hit)} onCopyPath={() => void act("copy", pv.hit)} />
      )}
      </div>

      {launchMenu && (
        <div className="ctx-overlay" onMouseDown={() => setLaunchMenu(null)}
             onContextMenu={(e) => { e.preventDefault(); setLaunchMenu(null); }}>
          <div className="ctx-menu" style={{ left: launchMenu.x, top: launchMenu.y }} role="menu" aria-label="Launcher actions"
               onMouseDown={(e) => e.stopPropagation()}>
            <button role="menuitem" onClick={() => { setLaunchMenu(null); void launchAct(launchMenu.hit, "open"); }}>Open</button>
            {launchMenu.hit.item.kind === "app" && launchMenu.hit.item.admin_ok && (
              <button role="menuitem" onClick={() => { setLaunchMenu(null); void launchAct(launchMenu.hit, "admin"); }}>Run as administrator</button>
            )}
            {launchMenu.hit.item.kind === "app" && (
              <button role="menuitem" onClick={() => { setLaunchMenu(null); void launchAct(launchMenu.hit, "reveal"); }}>Open file location</button>
            )}
            {launchMenu.hit.item.kind === "setting" && (
              <button role="menuitem" onClick={() => { setLaunchMenu(null); void launchAct(launchMenu.hit, "copy"); }}>Copy link</button>
            )}
            {launchMenu.hit.item.kind === "app" && (
              <button role="menuitem" onClick={() => { setLaunchMenu(null); void launchAct(launchMenu.hit, "copy"); }}>Copy target</button>
            )}
            <button role="menuitem" onClick={() => hideLaunch(launchMenu.hit)}>Hide from results</button>
          </div>
        </div>
      )}

      {cheat && <Shortcuts scope="popup" onClose={() => setCheat(false)} />}

      <footer className="footer">
        <span className={"status s-" + state}><i className="dot" />{statusText}</span>
        {models.running && (
          <button className="dl-chip" tabIndex={-1} title="Downloading the search-by-meaning models (one time). Click for details."
                  onMouseDown={(e) => e.preventDefault()} onClick={openSetup}>
            Setting up search by meaning{models.progress && models.progress.overall_total > 0
              ? ` · ${Math.min(100, Math.floor((100 * models.progress.overall_downloaded) / models.progress.overall_total))}%` : "…"}
          </button>
        )}
        {toast ? (
          <span className="toast" role="status">
            {toast.msg}
            {toast.undo && <> · <button className="undo" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => void doUndo()}>Undo</button></>}
          </span>
        ) : (
          !data.recent && loaded && <span className="count">
            {data.total.toLocaleString("en-US")}{data.partial ? "+" : ""} {overview && semHits.length ? "by name" : `result${data.total === 1 && !data.partial ? "" : "s"}`}
            {data.elapsedMs !== null && ` · ${data.elapsedMs < 1 ? "<1" : Math.round(data.elapsedMs)} ms`}
            {overview && semHits.length > 0 && ` · ${semHits.length} by meaning`}
          </span>
        )}
        <span className="spacer" />
        <span className="hints">
          {(pvOpen ? footerHints("preview") : footerHints(rowKind)).map(([id, verb]) => (
            <HintChip key={id + verb} id={id} verb={verb} />
          ))}
          <button className="lib-link" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={openLibrary} title={tooltip("popup.library")}>Library</button>
        </span>
      </footer>
    </div>
  );
}
