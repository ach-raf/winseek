import { Fragment, startTransition, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, DriveSpace, Filter, Hit, HitBox, Hotkeys, LaunchCfg, LaunchHit, ServiceState, Sort, VolumeStatus } from "./types";
import { backspaceDropsChip, hasRanges, nameQuery, parseQuery, rangeClause } from "./filters";
import { calc } from "./calc.ts";
import { FirstRun, type Summary } from "./FirstRun";
import { Setup } from "./Setup";
import { Settings, type SettingsSection } from "./Settings";
import { parseSection } from "./settingsNav";
import { isToggleKey, shellClass, toggleMode, type ViewMode } from "./viewMode";
import { Preview, type PreviewApi } from "./Preview";
import { LaunchMenu } from "./LaunchMenu";
import { SemanticHint, pathKey, semAccepts, whyName } from "./Semantic";
import { HitRow, Overview, LaunchPreview, launchVerb, type ItemHandlers } from "./Overview";
import { buildHome, buildOverview, bumpMru, chipCount, expandable, fmtMs, isGrid, onDrive, ovMove, scopeDrive, type Expand, type GridCols, type MruEntry, type OvItem } from "./overviewLogic";
import Library from "./Library";
import { Sidebar, WindowControls, type Page } from "./Sidebar";
import { IChevL, IExpand, IGear, IMag, IX } from "./icons";
import { portableApps, launchForFilter, makeLaunchGuard } from "./launchLogic";
import * as qh from "./queryHistory";
import { combos, matches as matchesKey } from "./keymap";
import { Shortcuts, tooltip } from "./Shortcuts";
import { useModels, useSemantic, useTier1 } from "./hooks";
import { kindOf, formatSize } from "./format";
import { pagesToEvict, scrollTopFor, vlist, vlistClipped } from "./vlist";

const ROW_H = 54;
const ROW_H_DENSE = 32; // compact rows in the full window: one line, name + location
// Rows fetched per request. One block covers the whole visible window on a scroll jump, and
// searchd caches the query's ordered results, so each further block is a cheap slice.
const PAGE = 400;
const OVERSCAN = 6;
// Result pages kept in memory (nearest the visible window win): a scrollbar sweep over millions of
// rows must not pile up every page it passed.
const PAGE_CAP = 60;

const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "files", label: "Files" },
  { id: "apps", label: "Apps" },
  { id: "settings", label: "Settings" }, // launcher-only scope: no name-index rows
  { id: "images", label: "Images" },
  { id: "docs", label: "Documents" },
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
// Every order, including the reversed ones the column headers reach (the chip cycles the four above).
const SORT_LABEL: Record<Sort, string> = {
  rel: "Best match", name: "Name", date: "Date modified", size: "Size",
  name_desc: "Name, Z to A", date_asc: "Oldest first", size_asc: "Smallest first", type: "Type, A to Z", type_desc: "Type, Z to A",
};
// Column headers of the full window: first click = `a`, a click on the active column flips to `b`.
const COLS: { label: string; a: Sort; b: Sort; up: Sort; hint: string; dense?: boolean }[] = [
  { label: "Name", a: "name", b: "name_desc", up: "name", hint: "Sort by name" },
  { label: "Type", a: "type", b: "type_desc", up: "type", hint: "Sort by type", dense: true },
  { label: "Modified", a: "date", b: "date_asc", up: "date_asc", hint: "Sort by date modified" },
  { label: "Size", a: "size", b: "size_asc", up: "size_asc", hint: "Sort by size" },
];

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
  // Sort order is remembered across sessions of the popup, so "Date modified" stays picked.
  const [sort, setSort] = useState<Sort>(() => {
    try {
      const v = localStorage.getItem("winseek.sort");
      return v && v in SORT_LABEL ? (v as Sort) : "rel";
    } catch {
      return "rel";
    }
  });
  const changeSort = useCallback((s: Sort) => {
    setSort(s);
    try { localStorage.setItem("winseek.sort", s); } catch { /* remembered only when storage works */ }
  }, []);
  const [sel, setSel] = useState(0);
  const [data, setData] = useState<Data>({ total: 0, rows: [], elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<VolumeStatus[]>([]);
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const [renaming, setRenaming] = useState(false); // inline rename of the selected row
  const [scrollTop, setScrollTop] = useState(0);
  // Exact list offset for wheel / keyboard moves on lists too long for the scrollbar to address (see vlist.ts).
  const [pin, setPin] = useState<number | null>(null);
  const [scrolling, setScrolling] = useState(false);
  const [loadErr, setLoadErr] = useState(false); // rows on screen could not be fetched (the service kept answering nothing)
  // Row density of the full window's file list, remembered.
  const [compactRows, setCompactRows] = useState(() => {
    try { return localStorage.getItem("winseek.density") === "compact"; } catch { return false; }
  });
  const toggleDensity = useCallback(() => {
    setCompactRows((c) => {
      try { localStorage.setItem("winseek.density", c ? "comfortable" : "compact"); } catch { /* not remembered */ }
      return !c;
    });
  }, []);
  const totalRef = useRef(0);
  const [space, setSpace] = useState<DriveSpace[]>([]);
  // Drive scope: a row of buttons while it fits, one chip with a menu in the popup or with many drives.
  const [driveOpen, setDriveOpen] = useState(false);
  const drivePickRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!driveOpen) return;
    const away = (e: MouseEvent) => { if (!drivePickRef.current?.contains(e.target as Node)) setDriveOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setDriveOpen(false); } };
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => { window.removeEventListener("mousedown", away); window.removeEventListener("keydown", esc, true); };
  }, [driveOpen]);
  const [liveVer, setLiveVer] = useState(0); // bumped when a live refresh found the results changed // shows the "row N of M" tag while the list moves
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
  // The last section is remembered across sessions of the popup.
  const [settingsSection, setSettingsSectionRaw] = useState<SettingsSection>(() => {
    try { return parseSection(localStorage.getItem("winseek.settingsSection")); } catch { return "general"; }
  });
  const setSettingsSection = useCallback((s: SettingsSection) => {
    setSettingsSectionRaw(s);
    try { localStorage.setItem("winseek.settingsSection", s); } catch { /* remembered only when storage works */ }
  }, []);
  // Compact popup or full-window layout; the window itself is resized by the backend.
  const [viewMode, setViewMode] = useState<ViewMode>("compact");
  const full = viewMode === "full";
  // Full window: which page the sidebar shows (the compact popup is always "search").
  const [page, setPage] = useState<Page>("search");
  // Drive scope (the segmented control): "" = all drives, else one letter.
  const [drive, setDrive] = useState("");
  // Per-type result counts of the typed query (the numbers on the filter chips).
  const [nameCounts, setNameCounts] = useState<Partial<Record<Filter, number>> | null>(null);
  // "Most used" apps on home: a launch count kept here (the backend has no usage listing).
  const [mru, setMru] = useState<MruEntry[]>(() => {
    try {
      const v = JSON.parse(localStorage.getItem("winseek.mru") ?? "[]");
      return Array.isArray(v) ? v.filter((e): e is MruEntry => !!e?.item?.id && typeof e.n === "number") : [];
    } catch { return []; }
  });
  const saveMru = useCallback((next: MruEntry[]) => {
    setMru(next);
    try { localStorage.setItem("winseek.mru", JSON.stringify(next)); } catch { /* remembered only when storage works */ }
  }, []);
  const [cheat, setCheat] = useState(false); // the F1 / Ctrl+/ shortcut cheat sheet
  const [hotkeyBindings, setHotkeyBindings] = useState<Record<string, string>>({});
  const [hintOff, setHintOff] = useState(false);
  const [summary, setSummary] = useState<Summary | null>(null);
  // Launcher (apps / settings / commands): hits per keystroke, the armed confirm row, the
  // in-app context menu, and the launcher config (the portable-exes toggle lives in it).
  const [launch, setLaunch] = useState<LaunchHit[]>([]);
  const [launchVer, setLaunchVer] = useState(0);
  const [lcfg, setLcfg] = useState<LaunchCfg | null>(null);
  const [armed, setArmed] = useState<string | null>(null);
  const [launchMenu, setLaunchMenu] = useState<{ hit: LaunchHit; x: number; y: number } | null>(null);
  const [menuIdx, setMenuIdx] = useState(0); // roving focus in the launcher menu
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setMenuIdx(0);
    // Opening with the keyboard must not steal the caret: focus the first item, the search box
    // gets it back when the menu closes (the global Esc handler calls refocus()).
    requestAnimationFrame(() => menuRef.current?.querySelector<HTMLElement>("[role=menuitem]")?.focus());
  }, [launchMenu]);
  const t0 = useRef<number | null>(null);
  const sawScan = useRef(false);
  const showPanelRef = useRef(false);
  const summaryRef = useRef<Summary | null>(null);
  const primaryRef = useRef<() => void>(() => {});
  const setupOpenRef = useRef(false);
  const setupPrimaryRef = useRef<() => void>(() => {});
  const ovRef = useRef<HTMLDivElement>(null);
  const previewApi = useRef<PreviewApi | null>(null); // the open preview's keyboard surface
  const openSettingsRef = useRef<() => void>(() => {}); // Settings: a panel in the popup, a page in the full window
  const enterRef = useRef<(ctrl: boolean, shift: boolean) => void>(() => {}); // Enter / Ctrl+Enter / Ctrl+Shift+Enter, also the footer buttons
  const menuAtRef = useRef<() => void>(() => {}); // the context menu at the selection (Menu key, Ctrl+K, "More")

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
  const pagesRef = useRef(new Set<number>()); // result pages present in rowsRef (paged lists only)
  const wantRef = useRef<[number, number]>([0, -1]); // rows the list is showing right now
  const emptyTries = useRef(new Map<number, number>());
  const pumpRef = useRef<() => void>(() => {});
  const pinRef = useRef<number | null>(null);
  const pinTop = useRef(-1); // the scrollTop we set for the current pin (a different one = the user dragged)
  const voffRef = useRef(0);
  const scrollTimer = useRef<number | undefined>(undefined);
  const rowsRef = useRef<(Hit | undefined)[]>([]);
  const toastTimer = useRef<number | undefined>(undefined);
  // One-level undo (last rename or recycle). Ctrl+Z triggers it only while the search box is empty or
  // unchanged since the action; otherwise Ctrl+Z is left to the browser (text undo).
  const undoRef = useRef<Undo | null>(null);
  const undoQuery = useRef("");
  const suppressClick = useRef(false);
  const launchOnce = useRef(makeLaunchGuard()).current;

  const isRecent = query.trim() === "" && filter === "all" && sort === "rel";

  // ---- natural filters -----------------------------------------------------
  // "photos from last summer" -> chips + free text. A removed chip's key goes into `off`; its words then count as text.
  const [off, setOff] = useState<ReadonlySet<string>>(new Set());
  const parsed = useMemo(() => parseQuery(query, { off }), [query, off]);
  useEffect(() => { if (!query.trim()) setOff((o) => (o.size ? new Set() : o)); }, [query]);
  const nameQ = nameQuery(parsed);
  // The date / size chips ride along as index terms, so they are exact over the whole index (no client scan).
  const scopedQ = `${scopeDrive(nameQ, drive)} ${rangeClause(parsed)}`.trim(); // what the name index gets: the words plus the drive scope
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
  const [t1, refreshT1] = useTier1(backend, setupOpen || settingsOpen || (full && page === "settings"), epoch);
  const models = useModels(backend, refreshT1);
  const t1Off = !!t1 && (t1.state === "models_missing" || t1.state === "unavailable");
  // Text found inside files (documents, PDFs, screenshot OCR) is searchable before the models exist, so the
  // meaning search runs whenever Tier 1 is up; without models it answers with text hits only.
  const [expand, setExpand] = useState<Expand>(null);
  const sem = useSemantic(backend, parsed.text, !!t1 && t1.state !== "unavailable" && !isRecent, effFilter, epoch, hasRanges(parsed) || !!parsed.ext, expand === "images" || expand === "docs" || effFilter === "images" || effFilter === "docs");
  // Meaning hits obey the same chips as the name results (kind / extension always; date / size when the hit carries them).
  const semHits = useMemo(() => sem.hits.filter((h) => semAccepts(h, parsed) && onDrive(h.path, drive)), [sem.hits, parsed, drive]);
  const semMap = useMemo(() => new Map(semHits.map((h) => [pathKey(h.path), h])), [semHits]);
  const selCount = data.total;

  // ---- Spotlight overview --------------------------------------------------
  // A typed search shows sections (Top Hit, Files, Images, Documents) with "Show all"; the recent list, a sort
  // other than best match, and "Show all files" use the full virtualised list instead.
  useEffect(() => { setExpand(null); }, [query, filter, drive]);
  // "Show all" on Photos: every picture whose name matches, not just the first page of the list (400 rows).
  // Read in 5,000-row pages; the grid fills as they arrive.
  const [photosAll, setPhotosAll] = useState<Hit[] | null>(null);
  const [photosBusy, setPhotosBusy] = useState(false);
  useEffect(() => {
    if (expand !== "images" || isRecent) { setPhotosAll(null); return; }
    let dead = false;
    (async () => {
      const out: Hit[] = [];
      for (;;) {
        const r = await backend.search({ query: scopedQ, offset: out.length, limit: 5000, sort: "name", filter: "images" });
        if (dead) return;
        for (const h of r.hits) out.push(h);
        const done = r.hits.length === 0 || out.length >= r.total;
        setPhotosAll(out.slice());
        setPhotosBusy(!done);
        if (done) break;
      }
    })().catch((e) => { if (!dead) setPhotosBusy(false); console.error(e); });
    return () => { dead = true; setPhotosBusy(false); };
  }, [backend, expand, isRecent, scopedQ, epoch]);
  // What the overview sections are built from: every row of a fully loaded list, else the first page. (The
  // paged row store is sparse and can span millions of indexes; nothing may loop over it.)
  const headRows = useMemo(
    () => (data.recent || data.full ? data.rows : data.rows.slice(0, PAGE)),
    [data.rows, data.recent, data.full],
  );
  const overview = sort === "rel" && expand !== "files"; // home (Most used + Recent files) is an overview too
  // The calculator Top Hit belongs to "all" only — a type filter means "show me that kind of file".
  const calcResult = useMemo(() => (settingsScope || expand || effFilter !== "all" ? null : calc(parsed.text)), [parsed.text, settingsScope, expand, effFilter]);
  // Launcher rows follow the type filter (a Folders search must not put a settings page in the Top Hit slot).
  const launchSel = useMemo(
    () => (drive ? [] : launchForFilter(launch, settingsScope ? "settings" : effFilter)), // apps / settings have no drive
    [launch, settingsScope, effFilter, drive], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const portable = useMemo(
    () => (lcfg?.portable === false ? [] : portableApps(headRows.filter((h): h is Hit => !!h), parsed.text, launch)),
    [lcfg, headRows, parsed.text, launch],
  );
  // Tile grids per layout: photos 4 / 5 columns, apps 6 / 8.
  const gridCols: GridCols = useMemo(() => ({ images: full ? 5 : 4, mostused: full ? 8 : 6 }), [full]);
  const mruHits = useMemo((): LaunchHit[] => (drive ? [] : mru.map((e) => ({ item: e.item, score: 0, ranges: [], confirm: false }))), [mru, drive]);
  const ov = useMemo(
    () => (isRecent
      ? buildHome({ mru: mruHits, recent: data.recent ? headRows : [], appCols: gridCols.mostused!, recentCap: full ? 40 : 7 })
      : buildOverview({
        names: settingsScope ? [] : photosAll ?? headRows, nameTotal: settingsScope ? 0 : data.total, sem: semHits, detached: !!photosAll, imageNameTotal: photosAll && !photosBusy ? undefined : nameCounts?.images, semMore: sem.more, text: parsed.text,
        expand: settingsScope ? "settings" : expand, isImage: (n) => kindOf(n, false) === "image", launch: launchSel, portable, calc: calcResult ?? undefined,
        photoCols: gridCols.images,
      })),
    [isRecent, mruHits, gridCols, full, data.recent, headRows, photosAll, photosBusy, nameCounts, data.total, semHits, sem.more, parsed.text, expand, launchSel, portable, calcResult, settingsScope], // eslint-disable-line react-hooks/exhaustive-deps
  );
  // The overview selection is kept by item key so results arriving later (meaning hits) do not move it.
  const [oselKey, setOselKey] = useState<string | null>(null);
  useEffect(() => { setOselKey(null); }, [query, filter, expand, drive]);
  const osel = Math.max(0, oselKey ? ov.flat.findIndex((x) => x.key === oselKey) : 0);
  const ovItem: OvItem | undefined = overview ? ov.flat[osel] : undefined;

  // ---- data loading --------------------------------------------------------
  useEffect(() => {
    const my = ++seq.current;
    inflight.current.clear();
    emptyTries.current.clear();
    setLoadErr(false);
    (async () => {
      if (isRecent) {
        const all = await backend.recent(drive ? 300 : 40);
        if (my !== seq.current) return;
        const hits = drive ? all.filter((h) => onDrive(h.path, drive)).slice(0, 40) : all;
        rowsRef.current = hits;
        setData({ total: hits.length, rows: hits, elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
      } else {
        const r = await backend.search({ query: scopedQ, offset: 0, limit: PAGE, sort, filter: effFilter });
        if (my !== seq.current) return;
        // Only the loaded rows are stored (`data.total` is the length); a one-letter query can match
        // millions of names and must not allocate a slot for each on every keystroke.
        const rows: (Hit | undefined)[] = r.hits.slice();
        rowsRef.current = rows;
        pagesRef.current = new Set([0]);
        setData({ total: r.total, rows, elapsedMs: r.elapsed_us / 1000, recent: false, full: false, partial: false, ver: 0 });
      }
      setLoaded(true);
      setSel(0);
      setScrollTop(0);
      pinRef.current = null;
      setPin(null);
      if (listRef.current) listRef.current.scrollTop = 0;
    })().catch(console.error);
  }, [backend, scopedQ, drive, effFilter, rangeKey, sort, isRecent, epoch]); // eslint-disable-line react-hooks/exhaustive-deps

  // Page loader: only ever fetches what the list shows *now*. While the scrollbar is dragged across millions
  // of rows the positions it passed are never requested (two requests at most are in flight; when one lands
  // the window is looked at again), so the rows under the thumb arrive at once instead of queueing behind
  // every page on the way.
  const pump = useCallback(() => {
    if (data.recent || data.full) return;
    const my = seq.current;
    const [f, l] = wantRef.current;
    for (let p = Math.floor(f / PAGE); p <= Math.floor(l / PAGE) && inflight.current.size < 2; p++) {
      if (pagesRef.current.has(p) || inflight.current.has(p) || p * PAGE >= data.total) continue;
      if ((emptyTries.current.get(p) ?? 0) >= 3) continue; // the service keeps answering nothing: stop asking
      inflight.current.add(p);
      backend.search({ query: scopedQ, offset: p * PAGE, limit: PAGE, sort, filter: effFilter }).then((r) => {
        if (my !== seq.current) return;
        inflight.current.delete(p);
        if (r.hits.length === 0) {
          const n = (emptyTries.current.get(p) ?? 0) + 1;
          emptyTries.current.set(p, n);
          if (n >= 3) setLoadErr(true);
        } else {
          const rows = rowsRef.current;
          r.hits.forEach((h, i) => (rows[p * PAGE + i] = h));
          pagesRef.current.add(p);
          const [f2, l2] = wantRef.current;
          for (const d of pagesToEvict(pagesRef.current, Math.floor(f2 / PAGE), Math.floor(l2 / PAGE), PAGE_CAP)) {
            pagesRef.current.delete(d);
            for (let i = d * PAGE; i < (d + 1) * PAGE; i++) delete rows[i];
          }
          setData((d) => ({ ...d, ver: d.ver + 1 }));
        }
        pumpRef.current();
      }).catch((e) => { if (my === seq.current) { inflight.current.delete(p); setLoadErr(true); } console.error(e); });
    }
  }, [backend, data.recent, data.full, data.total, scopedQ, sort, effFilter]);
  pumpRef.current = pump;

  const dense = full && compactRows && !overview;
  const rowH = dense ? ROW_H_DENSE : ROW_H;
  totalRef.current = data.total;
  const vl = vlist(data.total, rowH, scrollTop, viewH, OVERSCAN, pin);
  const first = vl.first;
  const last = vl.last;
  voffRef.current = vl.voff;
  useEffect(() => {
    wantRef.current = [first, last];
    if (last >= first) pump();
  }, [first, last, pump]);

  /** Put list offset `voff` (px into the whole list) at the top of the viewport, exactly. */
  const scrollToVoff = useCallback((voff: number) => {
    const el = listRef.current;
    if (!el) return;
    if (!vlistClipped(data.total, rowH)) { el.scrollTop = voff; return; }
    const v = Math.max(0, Math.min(voff, data.total * rowH - el.clientHeight));
    el.scrollTop = scrollTopFor(data.total, rowH, el.clientHeight, v);
    pinTop.current = el.scrollTop;
    pinRef.current = v;
    voffRef.current = v;
    setPin(v);
    setScrollTop(el.scrollTop);
  }, [data.total, rowH]);

  const onListScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    const st = e.currentTarget.scrollTop;
    // A scroll position we did not set = the user moved the scrollbar: back to the proportional mapping.
    if (pinRef.current !== null && Math.abs(st - pinTop.current) > 1.5) { pinRef.current = null; setPin(null); }
    setScrollTop(st);
    setScrolling(true);
    window.clearTimeout(scrollTimer.current);
    scrollTimer.current = window.setTimeout(() => setScrolling(false), 900);
  }, []);

  // Past the scrollbar's resolution the wheel is applied to the list offset directly (one notch must stay a
  // few rows, not a few thousand).
  useEffect(() => {
    const el = listRef.current;
    if (!el || !vlistClipped(data.total, rowH)) return;
    const onWheel = (e: WheelEvent) => {
      if (e.ctrlKey || e.deltaY === 0) return;
      e.preventDefault();
      const d = e.deltaMode === 1 ? e.deltaY * rowH : e.deltaMode === 2 ? e.deltaY * el.clientHeight : e.deltaY;
      scrollToVoff(voffRef.current + d);
      setScrolling(true);
      window.clearTimeout(scrollTimer.current);
      scrollTimer.current = window.setTimeout(() => setScrolling(false), 900);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [data.total, rowH, scrollToVoff]);

  // Keep the list true while it is open: every few seconds ask the index how many files match now (a count
  // costs it almost nothing). When that changed - a download finished, something was deleted - the rows are
  // refreshed in place; the selection and the scroll position stay where they are.
  useEffect(() => {
    if (isRecent || data.recent || !loaded || renaming) return;
    const my = seq.current;
    const args = { query: scopedQ, sort, filter: effFilter };
    const t = window.setInterval(() => {
      if (document.visibilityState !== "visible" || my !== seq.current) return;
      backend.search({ ...args, offset: 0, limit: 0 }).then((c) => {
        if (my !== seq.current || c.total === totalRef.current) return;
        if (c.total === 0 && c.elapsed_us === 0) return; // a superseded request, not an answer
        return backend.search({ ...args, offset: 0, limit: PAGE }).then((r) => {
          if (my !== seq.current || r.total !== c.total) return;
          const old = rowsRef.current;
          const rows: (Hit | undefined)[] = r.hits.slice();
          for (const k in old) { const j = Number(k); if (j >= PAGE && j < r.total) rows[j] = old[j]; } // shown until re-read
          rowsRef.current = rows;
          pagesRef.current = new Set([0]);
          emptyTries.current.clear();
          setData((d) => ({ ...d, total: r.total, rows, ver: d.ver + 1 }));
          setLiveVer((v) => v + 1); // the type-chip counts follow
        });
      }).catch(() => { /* the next tick asks again */ });
    }, 5000);
    return () => window.clearInterval(t);
  }, [backend, scopedQ, sort, effFilter, isRecent, data.recent, loaded, renaming]);

  // The numbers on the type chips: one limit-1 search per type for the typed query (debounced, newest wins).
  // Not for home, and not while a date / size filter is active (those are applied here, so the totals would lie).
  const countsOn = query.trim() !== "";
  useEffect(() => {
    if (!countsOn) { setNameCounts(null); return; }
    let dead = false;
    const t = window.setTimeout(() => {
      const kinds: Filter[] = ["all", "files", "images", "docs", "video", "audio", "folders"];
      // limit 0 = count only: the service neither builds rows nor caches a result list for these
      Promise.all(kinds.map((f) => backend.search({ query: scopedQ, offset: 0, limit: 0, sort: "rel", filter: f }).then((r) => [f, r.total] as const)))
        .then((rows) => { if (!dead) setNameCounts(Object.fromEntries(rows)); })
        .catch(() => { if (!dead) setNameCounts(null); });
    }, 140);
    return () => { dead = true; window.clearTimeout(t); };
  }, [backend, countsOn, scopedQ, epoch, liveVer]);

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

  useEffect(() => {
    let off: (() => void) | undefined;
    backend.onShown(() => {
      // Focus FIRST and synchronously: a hidden webview pauses rAF until it paints, so waiting for a
      // frame drops the first keystrokes. The refetch (epoch) is a low-priority transition behind it.
      inputRef.current?.focus();
      inputRef.current?.select();
      startTransition(() => setEpoch((e) => e + 1));
      if (summaryRef.current) setCounting(false); // the "indexed N files" line is one-shot
      requestAnimationFrame(() => { inputRef.current?.focus(); });
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend]);

  useEffect(() => { inputRef.current?.focus(); }, []);

  // View mode: follow the backend (it also announces the default view on every show), and F11 flips it.
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    backend.viewGet().then((s) => setViewMode(s.current)).catch(() => {});
    backend.onViewChanged(setViewMode).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend]);
  useEffect(() => { document.documentElement.classList.toggle("view-full", viewMode === "full"); }, [viewMode]);
  // Settings survives a view flip: the popup's Settings panel and the full-window Settings
  // page are the same surface, so flipping translates between the two representations
  // (`settingsOpen` in the popup, the "settings" page in the full window).
  useEffect(() => {
    if (viewMode === "full") {
      if (settingsOpen) { setSettingsOpen(false); setPage("settings"); }
    } else {
      setPage("search"); // the pages are a full-window thing
      if (page === "settings") setSettingsOpen(true);
    }
  }, [viewMode]); // eslint-disable-line react-hooks/exhaustive-deps — runs on mode flips only
  const flipView = useCallback(() => {
    void backend.viewSetCurrent(toggleMode(viewMode)).then(setViewMode).catch(() => {});
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [backend, viewMode]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (isToggleKey(e)) { e.preventDefault(); flipView(); } };
    window.addEventListener("keydown", onKey, true); // capture: works from inside Settings and the preview too
    return () => window.removeEventListener("keydown", onKey, true);
  }, [flipView]);

  // The cheat sheet shows the CURRENT global hotkey, not the default (PRD RB2).
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    const set = (h: Hotkeys) => setHotkeyBindings(h.popup ? { "global.toggle": h.popup } : {});
    backend.hotkeysGet().then(set).catch(() => {});
    backend.onHotkeysChanged(set).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend]);

  // The clipboard hotkey: the app shows the popup, we fill the box (PRD B9, P1).
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    backend.onSetQuery(({ query }) => {
      setQuery(query);
      requestAnimationFrame(() => { inputRef.current?.focus(); inputRef.current?.select(); });
    }).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend]);

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
    console.error("winseek action failed:", e);
    flash(String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, ""), false, 3500);
  }, [flash]);

  // Window drag: the bars are the handle (popup search bar, full-window top bar and side rail —
  // Settings / Library pages bring their own .tbar, so they drag the same way). A press on the bar
  // background starts the native drag right away, like a titlebar; buttons and inputs keep their
  // own behavior.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) return;
      const t = e.target as HTMLElement | null;
      if (!t || !t.closest(".sbar.pop, .tbar, .side")) return;
      if (t.closest("button, input, textarea, select, .rename")) return;
      e.preventDefault(); // a titlebar press: no text selection, no focus change
      backend.movePopup().catch(fail);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [backend, fail]);

  // Dragging from inside the search field needs a small move threshold, so a plain click still
  // places the caret and selects text; past it, the window drag takes over.
  useEffect(() => {
    let sx = 0, sy = 0;
    const cleanup = () => {
      window.removeEventListener("mousemove", mv);
      window.removeEventListener("mouseup", cleanup);
    };
    const mv = (e: MouseEvent) => {
      if (Math.hypot(e.clientX - sx, e.clientY - sy) <= 6) return;
      cleanup();
      backend.movePopup().catch(fail);
    };
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) return;
      const t = e.target as HTMLElement | null;
      if (!t || !t.closest(".sbar input")) return;
      sx = e.clientX; sy = e.clientY;
      window.addEventListener("mousemove", mv);
      window.addEventListener("mouseup", cleanup);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [backend, fail]);
  const refocus = useCallback(() => requestAnimationFrame(() => inputRef.current?.focus()), []);
  const openLibrary = useCallback(() => { backend.openLibrary().then(() => backend.hide()).catch(fail); }, [backend, fail]);
  /** Settings opened straight on the Status section (tray item, Ctrl+Shift+L). */
  const openStatus = useCallback(() => {
    setSetupOpen(false);
    setSettingsSection("status");
    if (full) { setSettingsOpen(false); setPage("settings"); } else setSettingsOpen(true);
    refreshT1();
  }, [refreshT1, full, setSettingsSection]);

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
        openSettingsRef.current();
        await backend.hide();
        return;
      }
      if (id === "cmd:winseek_library") { void backend.launchRecord(id, q); openLibrary(); return; }
      if (mode === "copy") { await backend.launchCopy(id); flash("Target copied"); return; }
      if (mode === "reveal") { await backend.launchReveal(id); await backend.hide(); return; }
      if (h.confirm && armed !== id) { armConfirm(id); return; } // second Enter confirms
      await backend.launchRun(id, q, mode === "admin");
      if (h.item.kind === "app") saveMru(bumpMru(mru, h.item, Date.now())); // "Most used" on home
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, parsed.text, armed, armConfirm, flash, fail, openLibrary, recordQuery, mru, saveMru]);

  const enter = (ctrl: boolean, shift: boolean) => {
    if (ovItem?.calc !== undefined) { // calculator result: Enter copies it
      void backend.copyTextHtml(ovItem.calc).then(() => backend.hide()).catch(fail);
      return;
    }
    if (ovItem?.launch) { void launchAct(ovItem.launch, ctrl && shift ? "admin" : ctrl ? "reveal" : "open"); return; }
    void act(ctrl ? "reveal" : "open", current);
  };
  enterRef.current = enter;

  const removeRow = useCallback((i: number) => {
    // The paged store is sparse (indexes up to the result count): shift only the rows that exist.
    const old = rowsRef.current;
    const rows: (Hit | undefined)[] = [];
    for (const k in old) {
      const j = Number(k);
      if (j < i) rows[j] = old[j];
      else if (j > i) rows[j - 1] = old[j];
    }
    rowsRef.current = rows;
    setData((d) => ({ ...d, rows, total: Math.max(0, d.total - 1) }));
    // Pages from the removed row on are one row short at their end; reload them once the index has seen
    // the change (asking right away could bring the removed file back).
    const my = seq.current, from = Math.floor(i / PAGE);
    window.setTimeout(() => {
      if (my !== seq.current) return;
      for (const p of [...pagesRef.current]) if (p >= from) pagesRef.current.delete(p);
      pumpRef.current();
    }, 1500);
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
      saveMru(mru.filter((e) => e.item.id !== h.item.id));
      pushUndo({ kind: "unhide", id: h.item.id }, "Hidden from results");
    }).catch(fail);
  }, [backend, pushUndo, fail, mru, saveMru]);

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
    const top = n * rowH, bottom = top + rowH, cur = voffRef.current;
    if (top < cur) scrollToVoff(top);
    else if (bottom + 8 > cur + el.clientHeight) scrollToVoff(bottom + 8 - el.clientHeight); // 8 = the list's top padding + air
  }, [data.total, selCount, scrollToVoff, rowH]);

  useEffect(() => {
    setSel((s) => (s >= selCount ? Math.max(0, selCount - 1) : s));
  }, [sel, selCount]);
  useEffect(() => {
    if (!overview) return;
    const el = ovRef.current?.querySelector('[data-sel="1"]');
    if (el) el.scrollIntoView({ block: "nearest" });
    else if (osel === 0) ovRef.current?.scrollTo({ top: 0 });
  }, [overview, osel, oselKey]);
  // Opening or leaving "Show all" starts at the top of the new list, wherever the old one was scrolled to.
  useEffect(() => { ovRef.current?.scrollTo({ top: 0 }); }, [expand]);
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
      if (launchMenu && e.key !== "Escape") return; // the launcher menu owns the keyboard (B5)
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
        void openMenu(sel, r.left + 120, r.top + 4 + sel * rowH - voffRef.current + rowH / 2);
      };
      menuAtRef.current = menuAtSelection;
      const ovStep = (dir: Parameters<typeof ovMove>[2]) => { e.preventDefault(); navigated.current = true; ovGo(ovMove(ov.flat, osel, dir, gridCols)); };
      // The cheat sheet is its own layer: F1 / Ctrl+/ toggles it, Esc closes it first of all.
      if (combos("popup.cheatsheet").some((c) => matchesKey(e, c))) { e.preventDefault(); setCheat((v) => !v); return; }
      if (e.key === "Escape" && cheat) { e.preventDefault(); setCheat(false); return; }
      // Full window, another page: the page owns the keyboard. Esc leaves Settings for the search.
      if (full && page !== "search") {
        if (e.key === "Escape" && page === "settings") { e.preventDefault(); setPage("search"); refocus(); }
        return;
      }
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
          changeSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + (e.shiftKey ? SORTS.length - 1 : 1)) % SORTS.length].id);
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
      if (matchesKey(e, "Ctrl+K")) { e.preventDefault(); menuAtSelection(); return; } // "More" in the footer
      // Ctrl+Shift+E opens the containing folder WITHOUT selecting (folder rows: the folder
      // itself); Ctrl+Shift+T opens a terminal there (PRD B4, P1). Launch rows: no-ops.
      if (matchesKey(e, "Ctrl+Shift+E") || matchesKey(e, "Ctrl+Shift+T")) {
        e.preventDefault();
        if (current && !ovItem?.launch) {
          const dir = current.is_dir || !current.path.includes("\\")
            ? current.path
            : current.path.slice(0, current.path.lastIndexOf("\\"));
          if (matchesKey(e, "Ctrl+Shift+E")) void backend.openPath(dir).then(() => backend.hide()).catch(fail);
          else void backend.openTerminal(dir).catch(fail);
        }
        return;
      }
      // While the preview is open its keys win: pages, zoom, pan, extract, copy text (PRD B6).
      if (pvOpen && previewApi.current) {
        const api = previewApi.current;
        const routes: [string, () => void][] = [
          ["Ctrl+PgDn", () => api.page(1)],
          ["Ctrl+PgUp", () => api.page(-1)],
          ["Ctrl+=", () => api.zoom(1.25)],
          ["Ctrl+-", () => api.zoom(0.8)],
          ["Ctrl+0", () => api.fit()],
          ["Ctrl+E", () => api.extract()],
          ["Ctrl+Shift+X", () => api.copyText()],
        ];
        const r = routes.find(([c]) => matchesKey(e, c));
        if (r) { e.preventDefault(); r[1](); return; }
        if (e.altKey && e.shiftKey && !e.ctrlKey) {
          const d = e.key === "ArrowLeft" ? [-1, 0] : e.key === "ArrowRight" ? [1, 0] : e.key === "ArrowUp" ? [0, -1] : e.key === "ArrowDown" ? [0, 1] : null;
          if (d) { e.preventDefault(); api.pan(d[0], d[1]); return; }
        }
      }
      if (overview && !e.altKey && !e.shiftKey) {
        // Ctrl+arrows / PageUp / PageDown jump between sections; left / right walk the image grid once the
        // keyboard is in the results (before that they move the caret in the search box).
        if (e.key === "ArrowDown") return ovStep(e.ctrlKey ? "next" : "down");
        if (e.key === "ArrowUp") return ovStep(e.ctrlKey ? "prev" : "up");
        if (e.key === "PageDown") return ovStep("next");
        if (e.key === "PageUp") return ovStep("prev");
        if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && navigated.current && isGrid(ovItem?.section)) {
          return ovStep(e.key === "ArrowLeft" ? "left" : "right");
        }
      }
      const pageRows = Math.max(1, Math.floor(viewH / rowH) - 1);
      if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "l") { e.preventDefault(); openLibrary(); return; }
      if (e.ctrlKey && e.key === ",") { e.preventDefault(); openSettingsRef.current(); return; }
      switch (e.key) {
        case "ArrowDown": e.preventDefault(); navigated.current = true; move(sel + 1); break;
        case "ArrowUp": e.preventDefault(); navigated.current = true; move(sel - 1); break;
        case "PageDown": e.preventDefault(); navigated.current = true; move(sel + pageRows); break;
        case "PageUp": e.preventDefault(); navigated.current = true; move(sel - pageRows); break;
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
          enterRef.current(e.ctrlKey, e.shiftKey);
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
  }, [move, sel, viewH, act, current, curIdx, query, pvOpen, filter, selCount, backend, renaming, flash, fail, doRecycle, doUndo, openMenu, openSemMenu, refreshT1, refocus, openLibrary, overview, ov.flat, osel, ovItem, ovGo, expand, pushUndo, launchAct, armed, launchMenu, cheat, openStatus, parsed.chips, dropChip, sort, recordQuery, launchMenu, refocus, pvOpen, full, page, gridCols]);

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
    if (full) setPage("search"); // the setup panel lives on the search page
    setSetupOpen(true);
    models.refreshPlan();
    refreshT1();
  }, [models, refreshT1, full]);
  const closeSetup = useCallback(() => { setSetupOpen(false); refocus(); }, [refocus]);
  const openSettings = useCallback(() => {
    setSetupOpen(false);
    if (full) { setSettingsOpen(false); setPage("settings"); } else setSettingsOpen(true);
    refreshT1();
  }, [refreshT1, full]);
  openSettingsRef.current = openSettings;
  const closeSettings = useCallback(() => { setSettingsOpen(false); setPage("search"); refocus(); }, [refocus]);
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
  const filesText = totalFiles >= 1e6 ? `${(totalFiles / 1e6).toFixed(1)}M` : totalFiles.toLocaleString("en-US");
  const statusText =
    state === "demo" ? `Demo data · ${service === "not_installed" ? "name service not installed" : service === "stopped" ? "name service stopped" : "searchd not running"}`
    : state === "offline" ? "searchd offline — reconnecting…"
    : totalFiles ? (state === "live" ? `${filesText} files indexed` : `${filesText} files · ${state}`)
    : state === "scanning" ? "Indexing…"
    : "Connecting…";
  const heading = data.recent
    ? "Recent"
    : query.trim() === ""
      ? (() => {
        const label = FILTERS.find((f) => f.id === filter)?.label ?? "Files";
        const where = drive ? ` on ${drive}:` : "";
        if (sort === "size" && filter === "folders") return `Biggest folders${where}`;
        if (sort === "rel" || sort === "date") return `Recent · ${label}${where}`;
        return `${label}${where} · ${SORT_LABEL[sort]}`;
      })()
      : null;
  const sortLabel = SORT_LABEL[sort];
  const nextSort = () => { changeSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + 1) % SORTS.length].id); inputRef.current?.focus(); };

  // Virtualised list (Show all files, sorted lists): the rows share the overview's row.
  const rowHandlers: ItemHandlers = useMemo(() => ({
    onSelect: (i) => {
      if (suppressClick.current) { suppressClick.current = false; return; }
      setSel(i); refocus();
    },
    onOpen: (i) => { if (!suppressClick.current) void act("open", rowsRef.current[i]); },
    onMenu: (i, e) => { e.preventDefault(); if (!rowsRef.current[i]) return; setSel(i); void openMenu(i, e.clientX, e.clientY); },
    onDown: (i, e) => { const h = rowsRef.current[i]; if (h) onRowDown(e, h); },
    onRenameCommit: (i, n) => void doRename(i, n),
    onRenameCancel: () => { setRenaming(false); refocus(); },
  }), [act, openMenu, onRowDown, doRename, refocus]);
  const visible = useMemo(() => {
    const out: React.ReactNode[] = [];
    for (let i = first; i <= last; i++) {
      const h = data.rows[i];
      if (!h) {
        out.push(
          <div key={i} className="row skeleton" style={{ transform: `translateY(${vl.top + i * rowH - vl.voff}px)` }}>
            <span className="ic" />
            <div className="tx"><span className="sk w1" /><span className="sk w2" /></div>
          </div>,
        );
        continue;
      }
      const why = data.recent ? "" : whyName(h, nameQ, chipLabels, semMap.get(pathKey(h.path))?.score);
      out.push(
        <HitRow key={i} hit={h} i={i} sel={i === sel} query={parsed.text} backend={backend} h={rowHandlers}
                renaming={renaming && i === sel} y={vl.top + i * rowH - vl.voff} title={why ? `${h.path}\n${why}` : h.path} />,
      );
    }
    return out;
  }, [first, last, vl.voff, vl.top, rowH, data.rows, data.ver, data.recent, parsed.text, nameQ, chipLabels, semMap, sel, backend, renaming, rowHandlers]);

  const ovHandlers: ItemHandlers = useMemo(() => ({
    onSelect: (i) => {
      if (suppressClick.current) { suppressClick.current = false; return; }
      ovGo(i); refocus();
    },
    onOpen: (i) => {
      const it = ov.flat[i];
      if (it?.calc !== undefined) { void backend.copyTextHtml(it.calc).then(() => backend.hide()).catch(fail); return; }
      // Launch rows never drag, so a stale drag flag must not swallow their click; one launch per id covers click + dblclick.
      if (it?.launch) { if (launchOnce(it.launch.item.id)) void launchAct(it.launch, "open"); return; }
      if (suppressClick.current) return;
      void act("open", it?.hit);
    },
    onMenu: (i, e) => {
      e.preventDefault();
      const it = ov.flat[i];
      if (!it || it.calc !== undefined) return;
      ovGo(i);
      if (it.launch) { setLaunchMenu({ hit: it.launch, x: e.clientX, y: e.clientY }); return; }
      if (it.nameIdx !== undefined) void openMenu(it.nameIdx, e.clientX, e.clientY); else void openSemMenu(it.hit.path, e.clientX, e.clientY);
    },
    onDown: (i, e) => { const it = ov.flat[i]; if (!it) return; if (it.launch) suppressClick.current = false; else onRowDown(e, it.hit); }, // launch rows do not drag
    onRenameCommit: (i, n) => { const idx = ov.flat[i]?.nameIdx; if (idx !== undefined) void doRename(idx, n); },
    onRenameCancel: () => { setRenaming(false); refocus(); },
  }), [ov.flat, ovGo, act, launchAct, launchOnce, openMenu, openSemMenu, onRowDown, doRename, refocus, backend, fail]);
  const showList = !overview;
  const waitingSem = overview && sem.query !== parsed.text.trim() && parsed.text.trim().length >= 2 && !!t1 && t1.state !== "unavailable";
  const pvPane = !showPanel && !!pv && !!current;
  const quickLook = pvOpen && pvPane;

  // Result stat in the search bar: "N results · 0.4 ms" (not on home).
  const statTotal = overview ? ov.sections.reduce((a, x) => a + x.total, 0) : data.total;
  const statEl = !showPanel && !isRecent && loaded ? (
    <span className="stat">
      {statTotal.toLocaleString("en-US")}{data.partial ? "+" : ""} result{statTotal === 1 && !data.partial ? "" : "s"}
      {data.elapsedMs !== null && <> {"·"} <b>{fmtMs(data.elapsedMs)} ms</b></>}
    </span>
  ) : null;

  // Free space per drive for the sidebar card (full window only; a minute old is fine).
  const driveKey = status.map((v) => v.letter).join("");
  useEffect(() => {
    if (!driveKey) return;
    let dead = false;
    const load = () => backend.driveSpace(driveKey.split("").filter((l) => /[A-Za-z]/.test(l))).then((s) => { if (!dead) setSpace(s); }).catch(() => { /* the card falls back to file counts */ });
    void load();
    const t = window.setInterval(load, 60000);
    return () => { dead = true; window.clearInterval(t); };
  }, [backend, driveKey]);
  // A click on a drive in that card: its folders, biggest first ("what is taking the space").
  const showDrive = (letter: string) => {
    setPage("search");
    setQuery("");
    setOff(new Set());
    setDrive(letter);
    setFilter("folders");
    changeSort("size");
    inputRef.current?.focus();
  };

  // ---- nothing found: the filters that are narrowing the search, each one click to lift ----
  const fixes: { label: string; run: () => void }[] = [];
  if (!isRecent) {
    if (drive) fixes.push({ label: "Search all drives", run: () => setDrive("") });
    if (effFilter !== "all") {
      fixes.push({ label: `Search all types, not only ${FILTERS.find((f) => f.id === effFilter)?.label ?? effFilter}`, run: () => {
        const k = parsed.chips.find((c) => c.type === "kind");
        if (k) dropChip(k.key);
        setFilter("all");
      } });
    }
    for (const c of parsed.chips) if (c.type !== "kind") fixes.push({ label: `Remove \u201c${c.label}\u201d`, run: () => dropChip(c.key) });
  }
  const fixEl = fixes.length > 0 ? (
    <div className="fixes">
      {fixes.map((f) => (
        <button key={f.label} className="fix" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => { f.run(); inputRef.current?.focus(); }}>{f.label}</button>
      ))}
    </div>
  ) : null;

  const driveMenu = !full || status.length > 3;

  // ---- filter row ----
  const countOf = (f: Filter): number | undefined => {
    if (!nameCounts) return undefined;
    const lc = (k: string) => (drive ? 0 : launch.filter((h) => h.item.kind === k).length);
    switch (f) {
      case "all": return (nameCounts.all ?? 0) + (drive ? 0 : launch.length);
      case "apps": return lc("app") + portable.length;
      case "settings": return lc("setting");
      default: return nameCounts[f];
    }
  };
  const frow = (
    <nav className="frow" role="tablist" aria-label="File type filter">
      {FILTERS.filter((f) => f.id !== "settings" || effFilter === "settings").map((f) => {
        const n = countOf(f.id);
        return (
          <button key={f.id} role="tab" aria-selected={effFilter === f.id} tabIndex={-1}
                  className={"fc" + (effFilter === f.id ? " on" : "") + (n === 0 ? " zero" : "")}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => {
                    const k = parsed.chips.find((c) => c.type === "kind");
                    if (k) dropChip(k.key); // a tab click overrides the kind found in the text
                    setFilter(f.id); inputRef.current?.focus();
                  }}>
            {f.label}{n !== undefined && n > 0 && <i title={n.toLocaleString("en-US")}>{chipCount(n)}</i>}
          </button>
        );
      })}
      <span className="sp" />
      {full && models.running && (
        <button className="sortb" tabIndex={-1} title="Downloading the search-by-meaning models (one time). Click for details."
                onMouseDown={(e) => e.preventDefault()} onClick={openSetup}>
          Setting up search by meaning{models.progress && models.progress.overall_total > 0
            ? ` · ${Math.min(100, Math.floor((100 * models.progress.overall_downloaded) / models.progress.overall_total))}%` : "…"}
        </button>
      )}
      {full && !isRecent && !overview && (
        <button className={"sortb dens" + (compactRows ? " on" : "")} tabIndex={-1} aria-pressed={compactRows}
                aria-label="Compact rows" title={compactRows ? "Compact rows are on. Click for roomy rows." : "Compact rows: more files on screen, with a Location column"}
                onMouseDown={(e) => e.preventDefault()} onClick={() => { toggleDensity(); inputRef.current?.focus(); }}>
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true"><path d="M4 6h16M4 10h16M4 14h16M4 18h16" /></svg>
        </button>
      )}
      {!isRecent && (
        <button className="sortb" tabIndex={-1} title={tooltip("popup.sort")} onMouseDown={(e) => e.preventDefault()} onClick={nextSort}>
          {sortLabel}
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
        </button>
      )}
      {status.length > 0 && (driveMenu ? (
        <div className="dpick" ref={drivePickRef}>
          <button className={"sortb" + (drive ? " set" : "")} tabIndex={-1} aria-haspopup="listbox" aria-expanded={driveOpen} title="Limit the search to one drive"
                  onMouseDown={(e) => e.preventDefault()} onClick={() => setDriveOpen((o) => !o)}>
            {drive ? `Drive ${drive}:` : "All drives"}
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
          </button>
          {driveOpen && (
            <div className="dmenu" role="listbox" aria-label="Drive">
              {[{ letter: "" }, ...status].map((v) => {
                const sp = space.find((x) => x.letter.toUpperCase() === v.letter.toUpperCase());
                return (
                  <button key={v.letter || "all"} role="option" aria-selected={drive === v.letter} className={drive === v.letter ? "on" : ""} tabIndex={-1}
                          onMouseDown={(e) => e.preventDefault()}
                          onClick={() => { setDrive(v.letter); setDriveOpen(false); inputRef.current?.focus(); }}>
                    <span>{v.letter ? `${v.letter}:` : "All drives"}</span>
                    {sp && <i>{formatSize(sp.free)} free</i>}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      ) : (
        <div className="dseg" title="Limit to one drive">
          <button className={!drive ? "on" : ""} tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => { setDrive(""); inputRef.current?.focus(); }}>All drives</button>
          {status.map((v) => (
            <button key={v.letter} className={drive === v.letter ? "on" : ""} tabIndex={-1} onMouseDown={(e) => e.preventDefault()}
                    onClick={() => { setDrive(v.letter); inputRef.current?.focus(); }}>{v.letter}:</button>
          ))}
        </div>
      ))}
    </nav>
  );

  // ---- search box (compact: the popup's top bar; full: the field in the top bar) ----
  const searchInner = (
    <>
      <IMag />
      {parsed.chips.map((c) => (
        <span key={c.key} className="qchip" title="Understood from your search">
          <span>{c.type}</span>{c.label}
          <button tabIndex={-1} aria-label={`Remove filter ${c.label}`} title="Remove this filter (its words are searched as text)"
                  onMouseDown={(e) => e.preventDefault()} onClick={() => dropChip(c.key)}>
            <IX />
          </button>
        </span>
      ))}
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => { histAt.current = -1; setQuery(e.target.value); }}
        placeholder="Search apps, files and photos"
        spellCheck={false}
        autoComplete="off"
        autoFocus
        aria-label="Search"
      />
      {query !== "" && (
        <button className="clr" tabIndex={-1} aria-label="Clear the search" title="Clear the search"
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => { setQuery(""); histAt.current = -1; inputRef.current?.focus(); }}>
          <IX size={13} />
        </button>
      )}
    </>
  );
  const winControls = <WindowControls onMin={() => void backend.minimize()} onRestore={flipView} onClose={() => void backend.hide()} />;

  // ---- footer actions (compact popup) ----
  const selItem = overview ? ovItem : undefined;
  const hasSel = overview ? !!ovItem : !!current;
  const isCalcSel = selItem?.calc !== undefined;
  const launchSelHit = selItem?.launch && !isCalcSel ? selItem.launch : undefined;
  const primary = isCalcSel ? "Copy result" : launchSelHit ? launchVerb(launchSelHit) : selItem?.section === "apps" ? "Launch" : "Open";
  const second: { label: string; keys: string[]; run: () => void } | null =
    launchSelHit ? (launchSelHit.item.kind === "app" && launchSelHit.item.admin_ok ? { label: "Run as admin", keys: ["Ctrl", "Shift", "↵"], run: () => enter(true, true) } : null)
    : isCalcSel || !hasSel ? null
    : { label: "Show in folder", keys: ["Ctrl", "↵"], run: () => enter(true, false) };
  const keep = (e: React.MouseEvent) => e.preventDefault(); // buttons must not take the caret from the box
  const actions = !showPanel && hasSel ? (
    <>
      <button className="act pri" tabIndex={-1} onMouseDown={keep} onClick={() => enter(false, false)}>{primary} <kbd>{"↵"}</kbd></button>
      {second && <><span className="vr" /><button className="act" tabIndex={-1} onMouseDown={keep} onClick={second.run}>{second.label} {second.keys.map((k) => <kbd key={k}>{k}</kbd>)}</button></>}
      {!isCalcSel && <button className="act" tabIndex={-1} onMouseDown={keep} onClick={() => menuAtRef.current()}>More <kbd>Ctrl</kbd><kbd>K</kbd></button>}
    </>
  ) : null;

  const toastEl = toast ? (
    <span className="toast" role="status">
      {toast.msg}
      {toast.undo && <> {"·"} <button className="undo" tabIndex={-1} onMouseDown={keep} onClick={() => void doUndo()}>Undo</button></>}
    </span>
  ) : null;

  const panels = showPanel && (setupOpen ? (
    <Setup t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing} onClose={closeSetup} primaryRef={setupPrimaryRef} />
  ) : settingsOpen ? (
    <Settings backend={backend} t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing}
              onSetup={openSetup} onClose={closeSettings} onHide={() => void backend.hide()} onLibrary={openLibrary}
              section={settingsSection} onSection={setSettingsSection} onError={fail} />
  ) : (
    <FirstRun
      mode={counting ? "counter" : "setup"} service={service} busy={setupBusy} error={setupErr}
      rows={status.filter((v) => v.state !== "demo")} summary={summary}
      onPrimary={() => void runSetup()}
      onDismiss={() => { setDismissed(true); setSetupErr(null); refocus(); }}
      onSemantic={t1?.state === "models_missing" ? openSetup : undefined}
    />
  ));

  return (
    <div className={"shell" + shellClass(viewMode) + (showPanel ? " panel" : "")}>
      {full && <Sidebar page={page} onPage={(p) => { setPage(p); if (p === "search") refocus(); }} status={status} space={space} onDrive={showDrive} />}

      <div className="main-col" hidden={full && page !== "search"}>
        {full ? (
          <header className="tbar">
            <div className="field sbar">{searchInner}</div>
            {statEl}
            <span className="sp" />
            {winControls}
          </header>
        ) : (
          <header className="sbar pop">
            {searchInner}
            {statEl}
            <button className="ibtn" tabIndex={-1} title={tooltip("popup.settings")} aria-label="Settings" onMouseDown={keep}
                    onClick={() => (settingsOpen ? closeSettings() : openSettings())}>
              <IGear size={17} />
            </button>
            <button className="ibtn" tabIndex={-1} title="Open full window (F11)" aria-label="Switch to the full window" onMouseDown={keep} onClick={flipView}>
              <IExpand />
            </button>
          </header>
        )}

        {/* a panel (Settings, setup, first run) has the popup to itself: the type filters belong to results */}
        {!showPanel && frow}

        {panels}

        <div className={"cols" + (quickLook ? " ql" : "")} hidden={showPanel}>
          <div className="lcol">
            {full && !isRecent && (
              <div className={"colh" + (dense ? " dense" : "")}>
                <span />
                {COLS.filter((c) => !c.dense || dense).map((c, ci) => {
                  const on = sort === c.a || sort === c.b;
                  return (
                    <Fragment key={c.label}>
                    <button tabIndex={-1} className={(on ? "on" : "") + (c.dense ? " ty" : "")} onMouseDown={keep}
                            aria-label={`${c.hint}${on ? `, now ${SORT_LABEL[sort]}` : ""}`}
                            title={on ? `${SORT_LABEL[sort]}. Click to reverse.` : c.hint}
                            onClick={() => { changeSort(sort === c.a ? c.b : c.a); inputRef.current?.focus(); }}>
                      {c.label}
                      {on && (
                        <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                          <path d={sort === c.up ? "m6 15 6-6 6 6" : "m6 9 6 6 6-6"} />
                        </svg>
                      )}
                    </button>
                    {dense && ci === 0 && <span>Location</span>}
                    </Fragment>
                  );
                })}
              </div>
            )}
            {!showPanel && showList && (heading || expand === "files") && (
              <div className="sh">
                {expand === "files" ? (
                  <button className="back" tabIndex={-1} onMouseDown={keep} onClick={() => setExpand(null)} aria-label="Back to all results">
                    <IChevL size={14} /><span>Files</span><i>{data.total.toLocaleString("en-US")}</i>
                  </button>
                ) : <span>{heading}</span>}
              </div>
            )}

            <div className={"list vlist" + (dense ? " dense" : "")} hidden={showPanel || !showList} ref={listRef} onScroll={onListScroll} role="listbox">
              <div className="spacer-v" style={{ height: vl.height }}>{visible}</div>
              {loaded && data.total === 0 && (
                <div className="none">
                  <b>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</b>
                  {fixes.length > 0 ? "Nothing matches with these filters on." : "Check the spelling, or try fewer words."}
                  {fixEl}
                </div>
              )}
            </div>

            {!showPanel && showList && scrolling && data.total > 2000 && (
              <div className="pos" aria-hidden="true">
                <b>{(Math.min(data.total, Math.floor(vl.voff / rowH) + 1)).toLocaleString("en-US")}</b> of {data.total.toLocaleString("en-US")}
              </div>
            )}

            {!showPanel && showList && loadErr && (
              <div className="lerr" role="alert">
                Some rows could not be loaded.
                <button tabIndex={-1} onMouseDown={keep} onClick={() => { emptyTries.current.clear(); setLoadErr(false); pumpRef.current(); }}>Try again</button>
              </div>
            )}

            {!showPanel && overview && (ov.flat.length > 0 ? (
              <Overview sections={ov.sections} sel={osel} renaming={renaming} query={parsed.text} backend={backend} expanded={!!expand} loading={sem.loading || photosBusy}
                        handlers={ovHandlers} onExpand={(id) => { if (id !== "top") setExpand(id as Expand); refocus(); }} onBack={() => { setExpand(null); refocus(); }} scrollRef={ovRef} armed={armed} />
            ) : loaded && !waitingSem ? (
              <div className="none">
                {isRecent ? (
                  <><b>Nothing here yet</b>Files you open or change show up here. Start typing to search.</>
                ) : (
                  <><b>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</b>{fixes.length > 0 ? "Nothing matches with these filters on." : "Check the spelling, or try describing what the file is about."}{fixEl}</>
                )}
              </div>
            ) : <div className="list ov" />)}

            {!showPanel && t1Off && !hintOff && query.trim() !== "" && (
              <SemanticHint onSetup={openSetup} onDismiss={() => setHintOff(true)} />
            )}
          </div>

          {/* full window: the preview is always there; compact: only as Quick Look (Space) */}
          {(full || quickLook) && (pvPane ? (
            <Preview backend={backend} hit={pv!.hit} query={parsed.text} snippet={pv!.snippet} page={pv!.page} boxes={pv!.boxes} apiRef={previewApi}
                     onOpen={() => void act("open", pv!.hit)} onReveal={() => void act("reveal", pv!.hit)} onCopyPath={() => void act("copy", pv!.hit)} />
          ) : (
            <aside className="pv-none">
              {ovItem?.calc !== undefined
                ? <div className="none"><b>Calculator</b>Press Enter to copy the result.</div>
                : isLaunchSel && ovItem?.launch
                ? <LaunchPreview backend={backend} l={ovItem.launch} />
                : <div className="none"><b>Nothing selected</b>Pick a result to preview it.</div>}
            </aside>
          ))}
        </div>

        {!full && (
          <footer className="abar">
            <span className="st"><span className={"dot" + (state === "live" ? "" : " warn")} />{statusText}</span>
            {models.running && (
              <button className="dl-chip" tabIndex={-1} title="Downloading the search-by-meaning models (one time). Click for details."
                      onMouseDown={keep} onClick={openSetup}>
                Setting up search by meaning{models.progress && models.progress.overall_total > 0
                  ? ` · ${Math.min(100, Math.floor((100 * models.progress.overall_downloaded) / models.progress.overall_total))}%` : "…"}
              </button>
            )}
            {toastEl}
            <span className="sp" />
            {actions}
          </footer>
        )}
      </div>

      {full && page === "settings" && (
        <div className="main-col">
          <Settings backend={backend} t1={t1} models={models} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing}
                    onSetup={openSetup} onClose={closeSettings} onHide={() => void backend.hide()} onLibrary={() => setPage("timeline")}
                    section={settingsSection} onSection={setSettingsSection} onError={fail} embedded chrome={winControls} />
        </div>
      )}
      {full && (page === "timeline" || page === "collections" || page === "cleanup") && (
        <div className="main-col">
          <Library backend={backend} embedded tab={page} onBack={() => { setPage("search"); refocus(); }} chrome={winControls} />
        </div>
      )}

      {launchMenu && <LaunchMenu
        menu={{ hit: launchMenu.hit, x: launchMenu.x, y: launchMenu.y }} idx={menuIdx} setIdx={setMenuIdx}
        listRef={menuRef}
        onRun={(mode) => { setLaunchMenu(null); void launchAct(launchMenu.hit, mode); }}
        onHide={() => hideLaunch(launchMenu.hit)}
        onClose={() => { setLaunchMenu(null); refocus(); }}
      />}

      {cheat && <Shortcuts scope="popup" bindings={hotkeyBindings} onClose={() => setCheat(false)} />}

      {full && toastEl && <div className="ftoast">{toastEl}</div>}
    </div>
  );
}
