import { Fragment, startTransition, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, DriveSpace, Filter, FsDelta, Hit, Hotkeys, LaunchCfg, LaunchHit, LaunchItem, ServiceState, Sort } from "./types";
import { SearchGuide } from "./SearchGuide";
import { backspaceDropsChip, hasRanges, kindScope, nameQuery, parseQuery, rangeClause } from "./filters";
import { partQuery, wordQuery } from "./pictureRank";
import { usePicChunks } from "./usePicChunks";
import { pathQueryFromPaste } from "./pathNorm";
import { fitMenuRect } from "./popupLogic";
import { previewSelectionText } from "./previewLogic";
import { calc } from "./calc.ts";
import { FirstRun, type Summary } from "./FirstRun";
import { Setup } from "./Setup";
import { Settings } from "./Settings";
import { isToggleKey, shellClass, toggleMode, type ViewMode } from "./viewMode";
import { Preview, type PreviewApi } from "./Preview";
import { LaunchMenu } from "./LaunchMenu";
import { KeyHints } from "./Hint";
import { SemanticHint, pathKey, semAccepts, whyName } from "./Semantic";
import { HitRow, Overview, LaunchPreview, launchVerb, type ItemHandlers } from "./Overview";
import { baseName, buildHome, buildOverview, bumpMru, meaningOnly, DEFAULT_HOME_QUERIES, EXAMPLE_QUERIES, HOME_ROWS, expandable, fmtMs, isGrid, matchPct, onDrive, ovKeyNear, ovMove, removeMru, restoreMru, scopeDrive, type Expand, type GridCols, type MruEntry, type OvItem } from "./overviewLogic";
import Library from "./Library";
import { Sidebar, WindowControls, type Page } from "./Sidebar";
import { IChevL, IExpand, IFilter, IGear, IMag, IX } from "./icons";
import { portableApps, launchForFilter, makeLaunchGuard } from "./launchLogic";
import * as qh from "./queryHistory";
import { combos, matches as matchesKey } from "./keymap";
import { Shortcuts, tooltip } from "./Shortcuts";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useDebouncedValue, useModels, usePaneFollow, useSemantic, useStoredSection, useStoredValue, useTier1 } from "./hooks";
import { qk } from "./qk";
import { JobsBar } from "./JobsBar";
import type { JobKey } from "./jobsLogic";
import { errText, kindOf, formatSize, chipCount } from "./format";
import { pagesToEvict, scrollTopFor, vlist, vlistClipped } from "./vlist";
import { noteScroll } from "./thumbQueue";
import { PIC_SEARCH_PAGE, readPictureNames } from "./pictureSearch";
import { flushSync } from "react-dom";
import { fsDelta, hasArrivals, hitAt, makeMatcher, pruneList, pruneRows, shiftIndex } from "./fsDelta";
import { pushLocalDelta } from "./fsDeltaPatch";
import { useFsDelta, useFsDeltaListener } from "./useFsDelta";

const ROW_H = 54;
const ROW_H_DENSE = 32; // compact rows in the full window: one line, name + location
// Rows fetched per request. One block covers the whole visible window on a scroll jump, and
// searchd caches the query's ordered results, so each further block is a cheap slice.
const PAGE = 400;
const OVERSCAN = 6;
// Result pages kept in memory (nearest the visible window win): a scrollbar sweep over millions of
// rows must not pile up every page it passed.
const PAGE_CAP = 60;
// Pictures whose name has the typed word, read for the overview's grid (two rows; the rest is "Show all").
const PIC_HEAD = 60;
// How far the opened "Other names containing" fold reads ahead of its scroll (pages of PIC_SEARCH_PAGE, capped
// at PIC_SEARCH_CAP). The opened grid itself reads by chunk near the viewport (usePicChunks), never ahead.
const PIC_PAGE = 2000;

const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "meaning", label: "By meaning" }, // meaning hits only: no name rows
  { id: "files", label: "Files" },
  { id: "apps", label: "Apps" },
  { id: "settings", label: "Settings" }, // launcher-only scope: no name-index rows
  { id: "images", label: "Pictures" },
  { id: "docs", label: "Documents" },
  { id: "video", label: "Video" },
  { id: "audio", label: "Audio" },
  { id: "folders", label: "Folders" },
];
const SORTS: { id: Sort; label: string }[] = [
  { id: "rel", label: "Best match" },
  { id: "name", label: "Name" },
  { id: "date", label: "Modified" }, // chip face; prose (titles, notes) uses SORT_LABEL's full "Date modified"
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

type Undo = { kind: "rename"; newPath: string; oldName: string } | { kind: "recycle"; paths: string[] } | { kind: "unhide"; id: string } | { kind: "mru"; entries: MruEntry[] };

interface Data {
  total: number; rows: (Hit | undefined)[]; elapsedMs: number | null; recent: boolean;
  /** Every row is loaded (a date / size filter was applied here); `partial` = the scan hit its cap. */
  full: boolean; partial: boolean;
  /** Bumped when pages are filled into `rows` in place (a copy per page would be O(rows loaded)). */
  ver: number;
}

/** "Most used" entries from storage; anything that is not a well-formed list is dropped, not cast. */
function readMru(raw: string | null): MruEntry[] {
  try {
    const v: unknown = JSON.parse(raw ?? "[]");
    return Array.isArray(v) ? v.filter((e): e is MruEntry => !!e?.item?.id && typeof e.n === "number") : [];
  } catch { return []; }
}

/** Collapsed kind sections from storage; only string keys with boolean values survive. */
function readKindFolds(raw: string | null): Record<string, boolean> {
  try {
    const v: unknown = JSON.parse(raw ?? "{}");
    if (!v || typeof v !== "object" || Array.isArray(v)) return {};
    const out: Record<string, boolean> = {};
    for (const [k, b] of Object.entries(v)) if (typeof b === "boolean") out[k] = b;
    return out;
  } catch { return {}; }
}

// ---- view transitions (evergreen WebView2): full-window page switches cross-fade (goPage) and
// Quick Look morphs the selected row into the preview pane (setQuickLook). Both skip silently for
// reduced motion or a hidden window, and any engine without the API just updates the DOM — the
// CLAUDE.md browser-support policy needs no fallbacks beyond that.
const canViewTransition = () =>
  typeof document.startViewTransition === "function"
  && document.visibilityState === "visible"
  && !matchMedia("(prefers-reduced-motion: reduce)").matches;
const EMPTY_SET: ReadonlySet<string> = new Set();

/** Open/closed + outside-click/Escape dismissal + the viewport clamp, shared by the filter row's
 *  two chip menus (drive, sort) so the pair cannot drift apart. The menu hangs off the chip's
 *  right edge; on a narrow (scaled or wrapped) row the chip can sit near the left edge, where
 *  right-anchoring would clip the menu out of the window, and a long list can run off the
 *  bottom — pull it fully inside once per open via fitMenuRect, the same clamp the ctx menu uses
 *  (the Filters popover solves the same problem from the other side). */
function useChipMenu() {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => { if (!wrapRef.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape" && !e.isComposing) { e.stopPropagation(); setOpen(false); } };
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => { window.removeEventListener("mousedown", away); window.removeEventListener("keydown", esc, true); };
  }, [open]);
  useEffect(() => {
    if (!open) return;
    const el = menuRef.current, wrap = wrapRef.current;
    if (!el || !wrap) return;
    const r = el.getBoundingClientRect(), wr = wrap.getBoundingClientRect();
    const fit = fitMenuRect(r.left, r.top, r.width, r.height, window.innerWidth, window.innerHeight);
    el.style.left = `${fit.left - wr.left}px`; // .dmenu is absolute inside .dpick: viewport → wrap coords
    el.style.top = `${fit.top - wr.top}px`;
    el.style.right = "auto";
  }, [open]);
  return { open, setOpen, wrapRef, menuRef };
}

export default function App({ backend }: { backend: Backend }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  // Sort order is remembered across sessions of the popup, so "Date modified" stays picked.
  const [sort, setSort] = useStoredValue<Sort>("winseek.sort",
    (v) => (v && v in SORT_LABEL ? (v as Sort) : "rel"), (s) => s);
  const [sel, setSel] = useState(0);
  const [data, setData] = useState<Data>({ total: 0, rows: [], elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
  const [loaded, setLoaded] = useState(false);
  const [toast, setToast] = useState<{ msg: string; undo: boolean } | null>(null);
  const [renaming, setRenaming] = useState(false); // inline rename of the selected row
  const [scrollTop, setScrollTop] = useState(0);
  // Exact list offset for wheel / keyboard moves on lists too long for the scrollbar to address (see vlist.ts).
  const [pin, setPin] = useState<number | null>(null);
  const [scrolling, setScrolling] = useState(false);
  const [loadErr, setLoadErr] = useState(false); // rows on screen could not be fetched (the service kept answering nothing)
  // A scope switch (type tab / drive / sort / date-size chips) is being fetched: the list shows
  // skeletons and the overview stays blank until the answer lands. Typing does NOT set this —
  // the name index answers in a few ms, and skeletons on every keystroke would flicker.
  const [pending, setPending] = useState(false);
  // A page-0 refetch while old rows stay on screen (typing): dims the stat chip when slow.
  const [refetching, setRefetching] = useState(false);
  // Row density of the full window's file list, remembered.
  const [compactRows, setCompactRows] = useStoredValue<boolean>("winseek.density",
    (v) => v === "compact", (c) => (c ? "compact" : "comfortable"));
  const toggleDensity = useCallback(() => setCompactRows(!compactRows), [setCompactRows, compactRows]);
  const totalRef = useRef(0);
  const [space, setSpace] = useState<DriveSpace[]>([]);
  // Drive scope: a row of buttons while it fits, one chip with a menu in the popup or with many drives.
  const driveCtl = useChipMenu();
  // Sort: the same chip + menu, one slot over — the filter row's select-box pair.
  const sortCtl = useChipMenu();
  const [liveVer, setLiveVer] = useState(0); // bumped when a live refresh found the results changed // shows the "row N of M" tag while the list moves
  const [viewH, setViewH] = useState(360);
  const [epoch, setEpoch] = useState(0); // bumped when the popup is re-shown
  // First run: panel shown while on demo data with the service missing/stopped. Typing hides it (clearing the
  // box brings it back); "Not now" hides it for the session. After install/start it becomes a live counter that
  // ends on the first keystroke once everything is live.
  const [dismissed, setDismissed] = useState(false);
  const [setupBusy, setSetupBusy] = useState(false);
  const [setupErr, setSetupErr] = useState<string | null>(null);
  const [counting, setCounting] = useState(false);
  // Search by meaning (Tier 1): setup panel, the "off" hint, semantic hits.
  const [setupOpen, setSetupOpen] = useState(false);
  // The preview pane always shows the selection (Spotlight); Space (`pvOpen`) blows it up to a full-window
  // Quick Look. `pv` is the settled hit it shows (see `usePaneFollow` below). `navigated`: the user has moved
  // through the results with the keyboard since the last keystroke in the box, so Space means "Quick Look",
  // not a space.
  const [pvOpen, setPvOpen] = useState(false);
  const navigated = useRef(false);
  const vtRun = useRef(0);
  /** Every Quick Look open/close goes through here: outside the animated path it is just setPvOpen,
   *  inside it the selected row and the pane swap one shared view-transition-name (`.vt-src` /
   *  `.vt-hero`, the same `pv-hero` name) so the row appears to become the preview and shrink back
   *  into its row on close. The classes exist only for the length of that one transition. */
  const setQuickLook = useCallback((open: boolean) => {
    if (!canViewTransition()) { setPvOpen(open); return; }
    const row = document.querySelector<HTMLElement>(".row.sel");
    const pane = document.querySelector<HTMLElement>(".pv");
    const run = ++vtRun.current;
    const cleanup = () => {
      if (run !== vtRun.current) return; // a newer morph owns the classes now
      document.querySelectorAll(".vt-src, .vt-hero").forEach((el) => el.classList.remove("vt-src", "vt-hero"));
    };
    if (!open) (pane ?? row)?.classList.add("vt-hero"); // closing: the pane holds the name in the old snapshot
    else row?.classList.add("vt-src"); // opening: the row holds it
    document.startViewTransition(() => {
      flushSync(() => setPvOpen(open));
      pane?.classList.remove("vt-hero"); // in the full window the pane persists docked; drop its old-state name
      row?.classList.remove("vt-src");
      (open ? document.querySelector<HTMLElement>(".pv") : document.querySelector<HTMLElement>(".row.sel"))
        ?.classList.add(open ? "vt-hero" : "vt-src");
    }).finished.finally(cleanup).catch(() => { /* a skipped transition rejects; cleanup already ran */ });
  }, []);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // The last section is remembered across sessions of the popup (the Library window shares the key).
  const [settingsSection, setSettingsSection] = useStoredSection();
  // Compact popup or full-window layout; the window itself is resized by the backend.
  const [viewMode, setViewMode] = useState<ViewMode>("compact");
  const full = viewMode === "full";
  // Full window: which page the sidebar shows (the compact popup is always "search").
  const [page, setPage] = useState<Page>("search");
  /** The one path for user-driven full-window page switches (clicks, Esc, tray items): outside the
   *  animated path a plain setPage, inside it the switch runs inside startViewTransition so the old
   *  page cross-fades into the new one. A re-pick of the current page stays a no-op — the drive
   *  cards re-trigger through setSpaceAt, not through a page change. */
  const goPage = useCallback((p: Page) => {
    if (p === page) return;
    if (!canViewTransition()) { setPage(p); return; }
    document.startViewTransition(() => flushSync(() => setPage(p)));
  }, [page]);
  // Once a library page has been opened, the Library stays mounted (hidden while other pages show):
  // remounting it refetched the whole image list, dup groups and collection results on every switch,
  // which is what made rapid moves between Search / Clean up / Collections feel broken.
  const [libSeen, setLibSeen] = useState(false);
  const libPage = page === "timeline" || page === "collections" || page === "cleanup";
  useEffect(() => { if (full && libPage) setLibSeen(true); }, [full, libPage]);
  // Drive scope (the segmented control): "" = all drives, else one letter.
  const [drive, setDrive] = useState("");
  // Per-type result counts of the typed query (the numbers on the filter chips).
  const [nameCounts, setNameCounts] = useState<Partial<Record<Filter, number>> | null>(null);
  // Which settled counts-key the numbers on screen belong to (stale detection dims them).
  const [countsFor, setCountsFor] = useState<string | null>(null);
  // "Most used" apps on home: a launch count kept here (the backend has no usage listing).
  const [mru, setMru] = useStoredValue<MruEntry[]>("winseek.mru", readMru, JSON.stringify);
  const [guide, setGuide] = useState(false); // the "Narrow your search" filter guide
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
  const [launchMenu, setLaunchMenu] = useState<{ hit: LaunchHit; x: number; y: number; home?: boolean } | null>(null);
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
  const [histList, setHistList] = useState<string[]>(() => hist.current); // mirror of `hist` for the home's "Recent searches"
  const histAt = useRef(-1); // -1 = the live input
  // "Try" examples: the user can dismiss them for good (the x on the row).
  const [tryHidden, setTryHidden] = useStoredValue<boolean>("winseek.tryHidden",
    (v) => v === "1", (h) => (h ? "1" : ""));
  const hideTry = useCallback(() => setTryHidden(true), [setTryHidden]);
  const recordQuery = useCallback(() => {
    const q = inputRef.current?.value ?? "";
    if (!q.trim()) return;
    hist.current = qh.push(hist.current, q);
    histAt.current = -1;
    qh.save(hist.current);
    setHistList(hist.current);
  }, []);
  const clearHistory = useCallback(() => {
    hist.current = [];
    histAt.current = -1;
    qh.save(hist.current);
    setHistList([]);
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
  const rowsRef = useRef<(Hit | undefined)[]>([]);
  const recheckRef = useRef<() => void>(() => {}); // the live count check: re-reads the first page when the count moved
  const toastTimer = useRef<number | undefined>(undefined);
  // One-level undo (last rename or recycle). Ctrl+Z triggers it only while the search box is empty or
  // unchanged since the action; otherwise Ctrl+Z is left to the browser (text undo).
  const undoRef = useRef<Undo | null>(null);
  const undoQuery = useRef("");
  const suppressClick = useRef(false);
  const launchOnce = useRef(makeLaunchGuard()).current;

  // The launcher home (Most used tiles) is the unscoped empty state only; a chosen drive turns the
  // empty box into an ordinary scope ("Recent · All files on D:"), with the sort chip and the
  // full window's column headers — not the USN recent ring, which re-read and reshuffled on
  // every file the drive touched.
  const isRecent = query.trim() === "" && !drive && filter === "all" && sort === "rel";
  // One effective sort everywhere: the queries, the chip, the cycle and the column headers all
  // read the same stored choice, so none of them can disagree with the list. A drive landing
  // with an empty box ranks by the tiers' rel fallback — the same order every other empty scope
  // (a filter tab with no query) already shows; nothing is substituted behind the user's back.
  const effSort: Sort = sort;
  const driveLanding = drive !== "" && query.trim() === "";
  // The orders the sort menu offers and Alt+S cycles: exactly SORTS on every surface — one
  // source of truth, no per-scope filtering. Best match is choosable on a drive landing like
  // anywhere else, and the pick sticks (it is the stored `sort`, never bounced back).
  const sortOpts = SORTS;
  const cycleSort = useCallback((dir: 1 | -1) => {
    setSort(sortOpts[(sortOpts.findIndex((s) => s.id === effSort) + dir + sortOpts.length) % sortOpts.length]!.id);
    inputRef.current?.focus();
  }, [sortOpts, effSort, setSort]);

  // ---- natural filters -----------------------------------------------------
  // "photos from last summer" -> chips + free text. A removed chip's key goes into `off`; its words then count as text.
  const [off, setOff] = useState<ReadonlySet<string>>(new Set());
  const parsed = useMemo(() => parseQuery(query, { off }), [query, off]);
  useEffect(() => { if (!query.trim()) setOff((o) => (o.size ? new Set() : o)); }, [query]);
  const nameQ = nameQuery(parsed);
  // The date / size chips ride along as index terms, so they are exact over the whole index (no client scan).
  const scopedQ = `${scopeDrive(nameQ, drive)} ${rangeClause(parsed)}`.trim(); // what the name index gets: the words plus the drive scope
  // "settings" is a launcher-only scope, not a name-index filter — via the word in the query or the tab.
  // One kind scope, whether it came from a word in the query ("cat image") or from the tab: both run the same code below.
  const effFilter: Filter = kindScope(parsed.kind, filter);
  const settingsScope = parsed.kind === "settings" || filter === "settings";
  // "By meaning" is a scope of its own: only the meaning index answers; the name index is asked as for "all"
  // (counts, the shared list loader) but its rows are never shown.
  const meaningScope = effFilter === "meaning";
  const nameFilter: Filter = meaningScope ? "all" : effFilter;
  const rangeKey = JSON.stringify([parsed.date, parsed.size]);
  const chipLabels = useMemo(() => parsed.chips.map((c) => c.label), [parsed]);
  const dropChip = useCallback((key: string) => {
    setOff((o) => new Set(o).add(key));
    inputRef.current?.focus();
  }, []);

  // ---- search by meaning ---------------------------------------------------
  const [t1, refreshT1] = useTier1(backend, setupOpen || settingsOpen || (full && page === "settings"));
  const models = useModels(backend, refreshT1);
  const t1Off = !!t1 && (t1.state === "models_missing" || t1.state === "unavailable");
  // Text found inside files (documents, PDFs, screenshot OCR) is searchable before the models exist, so the
  // meaning search runs whenever Tier 1 is up; without models it answers with text hits only.
  const [expand, setExpand] = useState<Expand>(null);
  const [weakOpen, setWeakOpen] = useState(false);
  // Files grouped into kind sections (Folders / Documents / …), remembered. The collapsed groups are
  // remembered too, so a group stays shut until it is opened again.
  const [groupByKind, setGroupByKind] = useStoredValue<boolean>("winseek.kindgroups",
    (v) => v === "1", (g) => (g ? "1" : "0"));
  const toggleGroupKind = useCallback(() => setGroupByKind(!groupByKind), [setGroupByKind, groupByKind]);
  const [kindFolds, setKindFolds] = useStoredValue<Record<string, boolean>>("winseek.kindfolds", readKindFolds, JSON.stringify);
  const toggleKindFold = useCallback((id: string) => setKindFolds({ ...kindFolds, [id]: !kindFolds[id] }), [setKindFolds, kindFolds]);
  const sem = useSemantic(backend, parsed.text, !!t1 && t1.state !== "unavailable" && !isRecent, nameFilter, epoch, hasRanges(parsed) || !!parsed.ext, expand === "images" || expand === "docs" || meaningScope || effFilter === "images" || effFilter === "docs");
  // Meaning hits obey the same chips as the name results (kind / extension always; date / size when the hit carries them).
  const semHits = useMemo(() => sem.hits.filter((h) => semAccepts(h, parsed, nameFilter) && onDrive(h.path, drive)), [sem.hits, parsed, nameFilter, drive]);
  const semMap = useMemo(() => new Map(semHits.map((h) => [pathKey(h.path), h])), [semHits]);
  const selCount = data.total;

  // ---- Spotlight overview --------------------------------------------------
  // A typed search shows sections (Top Hit, Files, Images, Documents) with "Show all"; the recent list, a sort
  // other than best match, and "Show all files" use the full virtualised list instead.
  useEffect(() => { setExpand(null); setWeakOpen(false); }, [query, filter, drive]);
  // Pictures asked for (the tab, or a kind word like "cat image") with something typed: the grid is the whole
  // ranked list, the very list "Show all" on the Pictures section opens.
  const picScope = effFilter === "images" && parsed.text.trim() !== "" && sort === "rel" && !settingsScope;
  const ovExpand: Expand = settingsScope ? "settings" : meaningScope ? "meaning" : expand ?? (picScope ? "images" : null);
  const picsOpen = ovExpand === "images";
  // The pictures whose name has the typed words as words ("cat.jpg", "my cat 2021.png", not "intricate.png"):
  // the index answers that exactly (`ws:` terms), so the grid never pages through mid-word matches to find them.
  // The overview reads the first of them; the opened grid reads them by position, a chunk at a time near the
  // viewport (usePicChunks), after the meaning matches it leaves out of that stream (`picExclude`).
  const picQ = `${scopeDrive(wordQuery(nameQ), drive)} ${rangeClause(parsed)}`.trim();
  const picWords = picQ !== scopedQ;
  const picsWanted = !isRecent && !settingsScope && effSort === "rel" && (effFilter === "all" || effFilter === "images");
  // The meaning matches lead the opened grid (ranked), so the name stream is paged without them: offsets stay exact.
  // Keyed by content: a new array of the same paths must not re-read the grid.
  const picExcludeKey = picsOpen ? semHits.filter((h) => kindOf(baseName(h.path), false) === "image").map((h) => h.path).join("\n") : "";
  const picExclude = useMemo(() => (picExcludeKey ? Array.from(new Set(picExcludeKey.split("\n"))) : []), [picExcludeKey]);
  const picGrid = usePicChunks(backend, { query: picQ, exclude: picExclude, on: picsOpen && picsWanted, epoch });
  // The fold under the opened grid reads as far as its own scroll has reached (`tailLimit`), never the whole library.
  const [tailLimit, setTailLimit] = useState(PIC_PAGE);
  const moreTail = useCallback(() => setTailLimit((n) => n + PIC_PAGE), []);
  useEffect(() => { setTailLimit(PIC_PAGE); }, [scopedQ, picsOpen, epoch]);
  const picAcc = useRef<{ key: string; hits: Hit[]; total: number }>({ key: "", hits: [], total: -1 });
  const [picNames, setPicNames] = useState<{ q: string; hits: Hit[]; total: number; done: boolean } | null>(null);
  useEffect(() => {
    // The opened grid has its own reader; only the overview's first rows are read here.
    if (!picsWanted || picsOpen) { setPicNames(null); return; }
    let dead = false;
    const t = window.setTimeout(async () => {
      try {
        const key = `${picQ}|${epoch}`;
        if (picAcc.current.key !== key) picAcc.current = { key, hits: [], total: -1 };
        const acc = picAcc.current;
        // The shared reader (pictureSearch), one page.
        await readPictureNames(backend, {
          query: picQ, stream: "pics", acc, limit: PIC_HEAD, page: PIC_HEAD,
          live: () => !dead,
          emit: (s) => setPicNames({ q: picQ, hits: acc.hits.slice(), total: s.total, done: s.done }),
        });
      } catch (e) {
        if (!dead) setPicNames((p) => p && { ...p, done: true });
        console.error(e);
      }
    }, 120);
    return () => { dead = true; window.clearTimeout(t); };
  }, [backend, picsWanted, picsOpen, picQ, epoch]);
  const picNamesNow = picNames?.q === picQ ? picNames : null;
  // The names that only hold the letters inside another word are read when their fold is opened, or when the
  // word search found nothing at all (then they are all there is to show). One typed word asks the index for
  // exactly those (`!ws:`), so a library of word matches is not read through to find them.
  const tailWanted = picsOpen && picWords && (weakOpen || picGrid.total === 0);
  const tailQ = `${scopedQ} ${partQuery(nameQ) ?? ""}`.trim();
  const [picTail, setPicTail] = useState<{ hits: Hit[]; done: boolean } | null>(null);
  const tailAcc = useRef<{ key: string; hits: Hit[]; total: number }>({ key: "", hits: [], total: -1 });
  useEffect(() => {
    if (!tailWanted) { setPicTail(null); return; }
    let dead = false;
    (async () => {
      const key = `${tailQ}|${epoch}`;
      if (tailAcc.current.key !== key) tailAcc.current = { key, hits: [], total: -1 };
      const acc = tailAcc.current;
      await readPictureNames(backend, {
        query: tailQ, stream: "tail", acc, limit: tailLimit, page: PIC_SEARCH_PAGE,
        live: () => !dead,
        emit: (s) => setPicTail({ hits: acc.hits.slice(), done: s.done }),
      });
    })().catch((e) => { if (!dead) setPicTail((p) => p && { ...p, done: true }); console.error(e); });
    return () => { dead = true; };
  }, [backend, tailWanted, tailQ, epoch, tailLimit]);
  const picsBusy = (picsOpen && picGrid.busy) || (!!picTail && !picTail.done);
  // What the overview sections are built from: every row of a fully loaded list, else the first page. (The
  // paged row store is sparse and can span millions of indexes; nothing may loop over it.)
  const headRows = useMemo(
    () => (data.recent || data.full ? data.rows : data.rows.slice(0, PAGE)),
    [data.rows, data.recent, data.full],
  );
  const overview = (sort === "rel" || meaningScope) && expand !== "files" && !(drive && query.trim() === ""); // a drive scope with an empty box lands on the plain sortable list, not grouped sections
  // The calculator Top Hit belongs to "all" only — a type filter means "show me that kind of file".
  const calcResult = useMemo(() => (settingsScope || expand || effFilter !== "all" ? null : calc(parsed.text)), [parsed.text, settingsScope, expand, effFilter]);
  // Launcher rows follow the type filter (a Folders search must not put a settings page in the Top Hit slot).
  const launchSel = useMemo(
    () => (drive ? [] : launchForFilter(launch, settingsScope ? "settings" : effFilter)), // apps / settings have no drive
    [launch, settingsScope, effFilter, drive],
  );
  const portable = useMemo(
    () => (lcfg?.portable === false ? [] : portableApps(headRows.filter((h): h is Hit => !!h), parsed.text, launch)),
    [lcfg, headRows, parsed.text, launch],
  );
  // Tile grids per layout: photos as many exact-size tiles as fit, most-used apps 6 / 7 (full mode shows 7 wide
  // Launchpad tiles; MRU_MAX still stores 12). Keep in sync with .grid.apps in search.css.
  // The pictures grid is the shared exact-size grid: its column count is what fits (Overview measures it).
  const [picCols, setPicCols] = useState(4);
  const gridCols: GridCols = useMemo(() => ({ images: picCols, namepics: picCols, mostused: full ? 7 : 6 }), [full, picCols]);
  // No history yet: a few generic launcher picks (well-known apps and settings), fetched once; never personal.
  const [defaultHits, setDefaultHits] = useState<LaunchHit[] | null>(null);
  const homeEmpty = isRecent && !drive && mru.length === 0;
  useEffect(() => {
    if (!homeEmpty || defaultHits) return;
    let dead = false;
    Promise.all(DEFAULT_HOME_QUERIES.map((q) => backend.launchSearch(q, 1).then((h) => h[0]).catch(() => undefined))).then((all) => {
      if (!dead) setDefaultHits(all.filter((h): h is LaunchHit => !!h).map((h) => ({ ...h, confirm: false })));
    });
    return () => { dead = true; };
  }, [homeEmpty, defaultHits, backend, launchVer]);
  const mruHits = useMemo((): LaunchHit[] => (drive ? [] : mru.length
    ? mru.map((e) => ({ item: e.item, score: 0, ranges: [], confirm: false }))
    : defaultHits ?? []), [mru, drive, defaultHits]);
  const ov = useMemo(
    () => (isRecent
      ? buildHome({ mru: mruHits, maxTiles: gridCols.mostused! * HOME_ROWS })
      : buildOverview({
        names: settingsScope || meaningScope ? [] : headRows, nameTotal: settingsScope || meaningScope ? 0 : data.total, sem: semHits, semMore: sem.more, reranked: sem.reranked, text: parsed.text,
        picNames: picNames?.hits, picTotal: picNamesNow?.total, picTail: picTail?.hits, picNameTotal: effFilter === "images" ? data.total : nameCounts?.images,
        expand: ovExpand, isImage: (n) => kindOf(n, false) === "image", launch: launchSel, portable, calc: calcResult ?? undefined,
        photoCols: gridCols.images, weakOpen, groupByKind, kindFolds,
        picGrid: picsOpen && picsWanted ? { total: picGrid.total, at: picGrid.at, view: picGrid.view } : undefined,
      })),
    [isRecent, mruHits, gridCols, headRows, picNames, picNamesNow, picTail, effFilter, nameCounts, data.total, semHits, sem.more, sem.reranked, parsed.text, ovExpand, launchSel, portable, calcResult, settingsScope, meaningScope, weakOpen, groupByKind, kindFolds, picsOpen, picsWanted, picGrid.total, picGrid.at, picGrid.view],
  );
  // The meaning matches the name index does not count, for the type chips (any view, not just the opened grid).
  const byMeaning = useMemo(() => (isRecent || settingsScope ? { picsByMeaning: 0, docsByMeaning: 0 } : meaningOnly(semHits, parsed.text, (n) => kindOf(n, false) === "image")),
    [isRecent, settingsScope, semHits, parsed.text]);
  // The overview selection is kept by item key so results arriving later (meaning hits) do not move it.
  const [oselKey, setOselKey] = useState<string | null>(null);
  useEffect(() => { setOselKey(null); }, [query, filter, expand, drive]);
  // A key that no longer resolves (rows left or moved while the live index updated) keeps its
  // POSITION instead of snapping to the top: on a busy index the next answer used to eat the
  // arrow press the user had just made, and the keyboard looked dead while stuck on row 0.
  const oselAt = useRef(0);
  const oselIdx = oselKey ? ov.nav.indexOf(oselKey) : -1;
  const osel = Math.max(0, oselIdx >= 0 ? oselIdx : Math.min(oselAt.current, ov.nav.length - 1));
  useEffect(() => { oselAt.current = osel; }, [osel]);
  // undefined also for a picture-grid position whose chunk is not in yet: nothing to preview or act on.
  const ovItem: OvItem | undefined = overview ? ov.nav.at(osel) : undefined;

  // ---- data loading --------------------------------------------------------
  const scopeRef = useRef<string | null>(null);
  useEffect(() => {
    const my = ++seq.current;
    inflight.current.clear();
    emptyTries.current.clear();
    setLoadErr(false);
    // A changed scope (type tab / drive / sort / date-size chips) shows skeletons right away: the
    // previous scope's rows must not sit under the new tab until the answer arrives. Typing keeps
    // the previous rows visible instead — per-keystroke skeletons would flicker.
    const sk = `${effFilter}\u0000${drive}\u0000${sort}\u0000${rangeKey}`;
    const scopeChanged = scopeRef.current !== null && scopeRef.current !== sk;
    if (scopeChanged) {
      setPending(true);
      setSel(0);
      pinRef.current = null;
      setPin(null);
      setScrollTop(0);
      if (listRef.current) listRef.current.scrollTop = 0;
    }
    scopeRef.current = sk;
    // Typing keeps the old rows painted: the stat chip dims instead (only when slow — see .stat.busy).
    if (!scopeChanged) setRefetching(true);
    (async () => {
      if (isRecent) {
        // The home is Most used tiles only: no recent-files read (no USN ring IPC).
        if (my !== seq.current) return;
        rowsRef.current = [];
        setData({ total: 0, rows: [], elapsedMs: null, recent: true, full: false, partial: false, ver: 0 });
      } else {
        const r = await backend.search({ query: scopedQ, offset: 0, limit: PAGE, sort: effSort, filter: nameFilter, stream: "list" });
        if (my !== seq.current) return;
        // Only the loaded rows are stored (`data.total` is the length); a one-letter query can match
        // millions of names and must not allocate a slot for each on every keystroke.
        const rows: (Hit | undefined)[] = r.hits.slice();
        rowsRef.current = rows;
        pagesRef.current = new Set([0]);
        setData({ total: r.total, rows, elapsedMs: r.elapsed_us / 1000, recent: false, full: false, partial: false, ver: 0 });
      }
      setLoaded(true);
      setPending(false);
      setRefetching(false);
      setSel(0);
      setScrollTop(0);
      pinRef.current = null;
      setPin(null);
      if (listRef.current) listRef.current.scrollTop = 0;
    })().catch(console.error);
  }, [backend, scopedQ, drive, nameFilter, effFilter, rangeKey, sort, effSort, isRecent, epoch]);

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
      backend.search({ query: scopedQ, offset: p * PAGE, limit: PAGE, sort: effSort, filter: nameFilter, stream: "list" }).then((r) => {
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
  }, [backend, data.recent, data.full, data.total, scopedQ, effSort, nameFilter]);
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

  const scrollPending = useRef(0);
  const scrollRaf = useRef(0);
  useEffect(() => () => cancelAnimationFrame(scrollRaf.current), []);
  const onListScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    const st = e.currentTarget.scrollTop;
    // A scroll position we did not set = the user moved the scrollbar: back to the proportional mapping.
    if (pinRef.current !== null && Math.abs(st - pinTop.current) > 1.5) { pinRef.current = null; setPin(null); }
    noteScroll(st);
    // One state update per frame: App re-renders on it, and a mouse wheel / touchpad can fire several
    // scroll events inside one frame.
    scrollPending.current = st;
    if (scrollRaf.current === 0) {
      scrollRaf.current = requestAnimationFrame(() => { scrollRaf.current = 0; setScrollTop(scrollPending.current); });
    }
    setScrolling(true);
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
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [data.total, rowH, scrollToVoff]);

  // The scroll state chip hides when scrolling truly ends: scrollend fires after the last scroll
  // event of the gesture, whatever drove it (wheel, scrollbar drag, clamped wheel-till writes),
  // so no debouncing timer is needed.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    // Clear on a zero timeout, not synchronously: a whole short gesture (a scrollbar flick, a clamped
    // wheel-till write) can fire its last scroll and its scrollend inside one rendering frame, and a
    // synchronous clear would batch true+false away before the chip ever committed. Timers are tasks,
    // ordered after React's scheduler task, so the visible state lands first.
    let tid = 0;
    const done = () => {
      if (tid === 0) tid = window.setTimeout(() => { tid = 0; setScrolling(false); }, 0);
    };
    el.addEventListener("scrollend", done);
    return () => { if (tid !== 0) window.clearTimeout(tid); el.removeEventListener("scrollend", done); };
  }, []);

  // Keep the list true while it is open: every few seconds ask the index how many files match now (a count
  // costs it almost nothing). When that changed - a download finished, something was deleted - the rows are
  // refreshed in place; the selection and the scroll position stay where they are.
  useEffect(() => {
    if (isRecent || data.recent || !loaded || renaming) return;
    const my = seq.current;
    const args = { query: scopedQ, sort: effSort, filter: nameFilter, stream: "list" as const };
    const tick = () => {
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
    };
    // A file-system delta that could bring new matches (something added / renamed in) asks at once (see onFsDelta).
    recheckRef.current = tick;
    const t = window.setInterval(tick, 5000);
    return () => { window.clearInterval(t); if (recheckRef.current === tick) recheckRef.current = () => {}; };
  }, [backend, scopedQ, sort, effSort, nameFilter, isRecent, data.recent, loaded, renaming]);

  // The numbers on the type chips: one searchd call answers all seven totals, once the query
  // has settled for 140 ms (newest wins). Not for home, and not while a date / size filter is
  // active (those are applied here, so the totals would lie).
  const countsOn = query.trim() !== "";
  const countsKey = useDebouncedValue(countsOn ? `${epoch}|${liveVer}|${scopedQ}` : null, 140);
  useEffect(() => {
    if (countsKey === null) { setNameCounts(null); setCountsFor(null); return; }
    let dead = false;
    const kinds: Filter[] = ["all", "files", "images", "docs", "video", "audio", "folders"];
    backend.kindCounts(scopedQ, kinds)
      .then((n) => { if (!dead) { setNameCounts(Object.fromEntries(kinds.map((f, i) => [f, n[i]]))); setCountsFor(countsKey); } })
      .catch(() => { if (!dead) setNameCounts(null); });
    return () => { dead = true; };
  }, [backend, countsKey, scopedQ, epoch]);
  // The chips keep the previous totals while the new ones are on their way; they dim once the
  // query on screen no longer matches the key the numbers were computed for.
  const countsWant = countsOn ? `${epoch}|${liveVer}|${scopedQ}` : null;
  const countsStale = nameCounts !== null && countsFor !== countsWant;

  // ---- status / window events ---------------------------------------------
  const fast = counting && !summary; // 250 ms while the first scan is being watched
  const qc = useQueryClient();
  // Volume status, polled: this is the same query entry the Library reads, and react-query's structural
  // sharing keeps an unchanged answer from re-rendering the popup (the old hand-rolled JSON.stringify
  // compare did this by hand — and re-rendered on any backend field reorder).
  const statusQ = useQuery({ queryKey: qk.libStatus, queryFn: () => backend.status(), refetchInterval: fast ? 250 : 1000 });
  const status = useMemo(() => statusQ.data ?? [], [statusQ.data]);
  // On demo data the first-run panel also needs the Windows service's install/run state; polled on the
  // same clock, only while it is relevant.
  const demoAll = status.length > 0 && status.every((v) => v.state === "demo");
  const serviceQ = useQuery({ queryKey: qk.service, queryFn: () => backend.serviceState(), refetchInterval: fast ? 250 : 1000, enabled: demoAll });
  const service: ServiceState | null = serviceQ.data ?? null;

  const startCounting = useCallback(() => {
    t0.current ??= performance.now();
    sawScan.current = false;
    setSummary(null);
    setCounting(true);
  }, []);

  // The window's one fs-delta subscription: deletes / renames / new files made anywhere (Explorer included)
  // patch every cached list in this window in place (the embedded Library, Space, meaning answers); the
  // result rows follow in `onFsDelta` below.
  useFsDelta(backend);

  useEffect(() => {
    let off: (() => void) | undefined;
    backend.onSourceChanged(() => {
      if (showPanelRef.current) startCounting();
      void qc.invalidateQueries({ queryKey: qk.libStatus }); // a file was deleted/moved from outside: status re-reads now
      setEpoch((e) => e + 1);
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend, qc, startCounting]);

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
  }, [viewMode]); // eslint-disable-line react-hooks/exhaustive-deps -- runs on mode flips only
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
      setQuery(pathQueryFromPaste(query) ?? query);
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
  }, [backend, parsed.text, launchVer]);

  // ---- actions -------------------------------------------------------------
  const flash = useCallback((msg: string, undo = false, ms = undo ? 5000 : 1600) => {
    setToast({ msg, undo });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), ms);
  }, []);
  const fail = useCallback((e: unknown) => {
    console.error("winseek action failed:", e);
    flash(errText(e), false, 3500);
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
      // Only empty bar background drags: the search field (icon, chips, text, clear), the filter row,
      // segmented tabs, chips and every control keep their own press (caret, text selection, click).
      // The guide panel (.sg) floats over the results and carries the scrollbar the user grabs — a
      // press anywhere in it (its scrollbar, its chips, its empty gaps) is never a titlebar press,
      // even in the popup where it hangs inside .sbar.pop (full mode already excluded it via .field).
      if (t.closest("button, input, textarea, select, a, [role=tab], [role=button], .rename, .field, .qchip, .dseg, .seg, .frow, .sg")) return;
      e.preventDefault(); // a titlebar press: no text selection, no focus change
      backend.movePopup().catch(fail);
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
    if (full) { setSettingsOpen(false); goPage("settings"); } else setSettingsOpen(true);
    refreshT1();
  }, [refreshT1, full, setSettingsSection, goPage]);

  /** The jobs strip: the job's own Settings section (meaning/models -> Search by meaning with its
   *  Pause button, everything else -> Status with the drive and parser detail). */
  const openJob = useCallback((job: JobKey) => {
    setSetupOpen(false);
    setSettingsSection(job === "meaning" || job === "models" ? "meaning" : "status");
    if (full) { setSettingsOpen(false); goPage("settings"); } else setSettingsOpen(true);
    refreshT1();
  }, [refreshT1, full, setSettingsSection, goPage]);

  const semSel = ovItem?.sem;
  const current: Hit | undefined = overview ? ovItem?.hit : data.rows[sel];
  /** Index of the selection in the name results, when it is one (rename / recycle / menu work on those rows). */
  const curIdx: number | undefined = overview ? ovItem?.nameIdx : current ? sel : undefined;

  const curSnippet = semSel?.snippet ?? (current ? semMap.get(pathKey(current.path))?.snippet : null) ?? null;
  const curPage = semSel?.page ?? (current ? semMap.get(pathKey(current.path))?.page : null) ?? null;
  const curLine = semSel?.line ?? (current ? semMap.get(pathKey(current.path))?.line : null) ?? null;
  const curBoxes = semSel?.boxes ?? (current ? semMap.get(pathKey(current.path))?.boxes : null) ?? null;
  const curAt = semSel?.at ?? (current ? semMap.get(pathKey(current.path))?.at : null) ?? null;
  // A picture tile carries its own final score (meaning + name); everything else the meaning hit's.
  const curPct = ovItem?.pct !== undefined ? ovItem.pct : matchPct(semSel ?? (current ? semMap.get(pathKey(current.path)) : undefined));
  const curKey = current ? `${current.path}|${curSnippet ?? ""}|${curPage ?? ""}|${curLine ?? ""}|${curAt ?? ""}|${curBoxes?.length ?? 0}|${curPct ?? ""}` : "";
  const pctRef = useRef(curPct);
  pctRef.current = curPct;
  const currentRef = useRef(current);
  currentRef.current = current;
  const snippetRef = useRef(curSnippet);
  snippetRef.current = curSnippet;
  const pageRef = useRef(curPage);
  pageRef.current = curPage;
  const lineRef = useRef(curLine);
  lineRef.current = curLine;
  const boxesRef = useRef(curBoxes);
  boxesRef.current = curBoxes;
  const atRef = useRef(curAt);
  atRef.current = curAt;
  // The pane follows the selection ~120 ms after it settles (arrowing through a list must not load every
  // file); the first pick after an empty pane shows at once. Launch rows (apps / settings / commands)
  // have no preview: the pane stays empty for them. (Gallery's lead preview runs the same hook.)
  const isLaunchSel = !!(overview && ovItem?.launch);
  const pv = usePaneFollow(current && !isLaunchSel ? curKey : null, 120, () => currentRef.current && ({
    hit: currentRef.current, snippet: snippetRef.current, page: pageRef.current,
    at: atRef.current, boxes: boxesRef.current, pct: pctRef.current,
  }));
  useEffect(() => { navigated.current = false; }, [query]);

  const armTimer = useRef<number | undefined>(undefined);
  const armConfirm = useCallback((id: string) => {
    window.clearTimeout(armTimer.current);
    setArmed(id);
    armTimer.current = window.setTimeout(() => setArmed(null), 4000);
  }, []);
  useEffect(() => { setArmed(null); }, [oselKey, query]); // selection / query change disarms

  // Recycle asks twice (the launcher commands' second-Enter pattern): the first Delete arms for
  // 4 s, the second one recycles. The message rides the toast; Esc or a moved selection disarms.
  const delTimer = useRef<number | undefined>(undefined);
  const [delArm, setDelArm] = useState(false);
  const armDel = useCallback(() => {
    window.clearTimeout(delTimer.current);
    setDelArm(true);
    delTimer.current = window.setTimeout(() => setDelArm(false), 4000);
  }, []);
  const disarmDel = useCallback(() => {
    window.clearTimeout(delTimer.current);
    setDelArm(false);
  }, []);
  useEffect(() => { disarmDel(); }, [current?.path, query, disarmDel]);

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
      // A Most-used tile for a file-backed app the catalog no longer holds (moved / removed
      // since): open its path directly instead of failing on an unknown id.
      const tp = h.item.target;
      if (tp.t === "path" && !launch.some((l) => l.item.id === id)) {
        void backend.launchRecord(id, q);
        if (h.item.kind === "app" || h.item.kind === "setting") setMru(bumpMru(mru, h.item, Date.now()));
        await backend.openPath(tp.path);
        await backend.hide();
        return;
      }
      await backend.launchRun(id, q, mode === "admin");
      if (h.item.kind === "app" || h.item.kind === "setting" || (h.item.kind === "command" && !h.confirm)) setMru(bumpMru(mru, h.item, Date.now())); // "Most used" on home
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, parsed.text, armed, armConfirm, flash, fail, openLibrary, recordQuery, mru, setMru, launch]);

  const act = useCallback(async (kind: "open" | "reveal" | "copy", h: Hit | undefined) => {
    if (!h) return;
    recordQuery();
    try {
      if (kind === "copy") { await backend.copyPath(h.path); flash("Path copied"); return; }
      if (kind === "open") {
        // An .exe opened from results is an app being launched, so it takes the app path: the
        // launcher catalog's entry (same id as the backend's portable scan, `app:<path>`) goes
        // through launchAct — launch history, frecency and "Most used" — and an exe the catalog
        // does not hold yet is recorded under that same id, so the next scan knows the habit
        // and the tile shows up. Every open (Enter, double-click, Open button) lands here.
        if (/\.exe$/i.test(h.name)) {
          const cat = launch.find((l) => l.item.target.t === "path" && l.item.target.path.toLowerCase() === h.path.toLowerCase());
          if (cat) { await launchAct(cat, "open"); return; }
          const stem = h.name.replace(/\.exe$/i, "");
          const item: LaunchItem = {
            id: "app:" + h.path.toLowerCase(), kind: "app", title: stem,
            subtitle: "Portable · " + h.path, keywords: [stem.toLowerCase()],
            target: { t: "path", path: h.path, args: null }, icon: null, admin_ok: true,
          };
          setMru(bumpMru(mru, item, Date.now())); // "Most used" on home
          void backend.launchRecord(item.id, parsed.text);
        }
        // A PDF hit found on a page opens on that page when the viewer can be steered; a code hit
        // with a line opens there (VS Code / Notepad++); anything else opens the plain way.
        const pg = /\.pdf$/i.test(h.path) ? pageRef.current : null;
        const ln = pg ? null : lineRef.current;
        if (pg && pg > 0) await backend.openAt(h.path, { page: pg });
        else if (ln && ln > 0) await backend.openAt(h.path, { line: ln });
        else await backend.openPath(h.path);
      } else await backend.revealPath(h.path);
      await backend.hide();
    } catch (e) { fail(e); }
  }, [backend, flash, fail, recordQuery, launch, launchAct, mru, setMru, parsed.text]);

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

  // ---- file-system deltas (anything changed on disk, here or in Explorer) ----------------------------
  // Gone rows leave and renamed rows move in place (the same shift the own delete does); the selection
  // stays on its row, or on the one that took its place. New files can only enter through the index:
  // the live count check runs once (debounced), never a re-read of every loaded page.
  const reselectAt = useRef<number | null>(null);
  const arrivalTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(arrivalTimer.current), []);
  // Rows that just arrived through the live index (fs-delta added / renamed-in) flash in once; the
  // set expires so scrolled-back rows do not re-animate.
  const freshTimer = useRef<number | undefined>(undefined);
  const [fresh, setFresh] = useState<ReadonlySet<string>>(EMPTY_SET);
  useEffect(() => () => window.clearTimeout(freshTimer.current), []);

  const onFsDelta = (d: FsDelta) => {
    const m = makeMatcher(d);
    if (!m.idle) {
      const r = pruneRows(rowsRef.current, m);
      if (r.changed) {
        rowsRef.current = r.rows;
        const n = r.removed.length;
        const total = Math.max(0, data.total - n);
        setData((x) => ({ ...x, rows: r.rows, total: Math.max(0, x.total - n), ver: x.ver + 1 }));
        if (n > 0) {
          setSel((s) => shiftIndex(s, r.removed, total));
          if (!data.recent) {
            // the loaded pages from the first removed row on are short at their ends: re-read the ones on screen once the index has it
            const my = seq.current, from = Math.floor(r.removed[0]! / PAGE);
            window.setTimeout(() => {
              if (my !== seq.current) return;
              for (const p of [...pagesRef.current]) if (p >= from) pagesRef.current.delete(p);
              pumpRef.current();
            }, 1500);
          }
        }
      }
      const prune = (hits: Hit[]) => pruneList(hits, (h) => h.path, m, hitAt);
      picAcc.current = { ...picAcc.current, hits: prune(picAcc.current.hits) };
      tailAcc.current = { ...tailAcc.current, hits: prune(tailAcc.current.hits) };
      setPicNames((p) => { if (!p) return p; const hits = prune(p.hits); return hits === p.hits ? p : { ...p, hits, total: Math.max(0, p.total - (p.hits.length - hits.length)) }; });
      // The opened grid's chunks are read again once the index has the change (its positions moved).
      if (picsOpen) window.setTimeout(picGrid.refresh, 1500);
      setPicTail((p) => { if (!p) return p; const hits = prune(p.hits); return hits === p.hits ? p : { ...p, hits }; });
      // The overview's selected item left (or moved): the selection goes to whatever takes its place.
      const sp = overview ? ovItem?.hit?.path : undefined;
      if (sp && (m.isGone(sp) || m.rewrite(sp) !== sp)) reselectAt.current = osel;
    }
    if (d.overflow || hasArrivals(d)) {
      if (hasArrivals(d)) {
        const added = [...d.added, ...d.renamed.map((rn) => rn.to)].slice(0, 500); // capped: a bulk copy cannot grow the set unbounded
        window.clearTimeout(freshTimer.current);
        setFresh(new Set(added));
        freshTimer.current = window.setTimeout(() => setFresh(EMPTY_SET), 2200);
      }
      window.clearTimeout(arrivalTimer.current);
      arrivalTimer.current = window.setTimeout(() => {
        if (!data.recent) recheckRef.current(); // the home lists no files, so nothing to re-read
      }, 300);
    }
  };
  useFsDeltaListener(onFsDelta);
  useEffect(() => {
    const at = reselectAt.current;
    if (at === null) return;
    reselectAt.current = null;
    setOselKey(ov.nav.keyAt(Math.min(at, ov.nav.length - 1)) ?? null);
  }, [ov.nav]);
  // The app's own recycle: every list in this window drops the files at once (the backend's delta follows and is a no-op).
  const ownRemoved = useCallback((paths: string[]) => pushLocalDelta(qc, fsDelta({ removed: paths })), [qc]);

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
      ownRemoved([h.path]);
      pushUndo({ kind: "recycle", paths: [h.path] }, "Moved to Recycle Bin");
    } catch (e) { fail(e); }
  }, [backend, removeRow, ownRemoved, pushUndo, fail]);

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
      pushLocalDelta(qc, fsDelta({ renamed: [{ from: h.path, to: np }] })); // the Library / Space / meaning lists follow
      pushUndo({ kind: "rename", newPath: np, oldName: h.name }, "Renamed");
    } catch (e) { fail(e); }
  }, [backend, qc, pushUndo, fail, refocus]);

  const doUndo = useCallback(async () => {
    const u = undoRef.current;
    if (!u) return;
    undoRef.current = null;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    try {
      // File undos go through the same delta path as any change on disk: the row moves back in place, a restored
      // file re-enters through the live count check (one first-page read) - no re-run of the whole search.
      if (u.kind === "rename") {
        const back = await backend.renamePath(u.newPath, u.oldName);
        pushLocalDelta(qc, fsDelta({ renamed: [{ from: u.newPath, to: back }] }));
        flash("Rename undone");
      } else if (u.kind === "unhide") {
        await backend.launchUnhide(u.id);
        setLaunchVer((v) => v + 1);
        flash("Back in the results");
        setEpoch((e) => e + 1); // launcher rows are not files: re-run the search
      } else if (u.kind === "mru") {
        setMru(restoreMru(mru, u.entries));
        flash("Back in Most used");
      } else {
        const n = await backend.restoreRecycled(u.paths);
        pushLocalDelta(qc, fsDelta({ added: u.paths }));
        flash(n > 0 ? "Restored" : "Nothing to restore");
      }
    } catch (e) { fail(e); }
  }, [backend, qc, flash, fail, mru, setMru]);

  const hideLaunch = useCallback((h: LaunchHit) => {
    setLaunchMenu(null);
    void backend.launchHide(h.item.id).then(() => {
      setLaunch((prev) => prev.filter((x) => x.item.id !== h.item.id));
      setMru(removeMru(mru, h.item.id));
      pushUndo({ kind: "unhide", id: h.item.id }, "Hidden from results");
    }).catch(fail);
  }, [backend, pushUndo, fail, mru, setMru]);

  /** "Remove from this list" on a Most-used tile: drop the entry and its launch weight from the
   *  home history (no results hide — launching it again counts 1 and brings the tile back). */
  const removeLaunchMru = useCallback((h: LaunchHit) => {
    setLaunchMenu(null);
    const gone = mru.find((e) => e.item.id === h.item.id);
    if (!gone) return; // not in the history (defaults or a stale menu): nothing to remove
    setMru(removeMru(mru, h.item.id));
    pushUndo({ kind: "mru", entries: [gone] }, "Removed from Most used");
  }, [mru, setMru, pushUndo]);

  const openMenu = useCallback(async (i: number, x: number, y: number) => {
    const h = rowsRef.current[i];
    if (!h) return;
    try {
      const r = await backend.showContextMenu(h.path, x, y);
      if (r === "rename") setRenaming(true);
      else if (r === "deleted") { removeRow(i); ownRemoved([h.path]); pushUndo({ kind: "recycle", paths: [h.path] }, "Moved to Recycle Bin"); }
      else if (r === "invoked") { await backend.hide(); return; } // Open / Open with / Send to... moved on to another app
    } catch (e) { fail(e); }
    refocus();
  }, [backend, removeRow, ownRemoved, pushUndo, fail, refocus]);

  // Row drag out of the list: pointer capture on the row itself, like the image panner. The
  // move/up events keep arriving outside the window, and nothing leaks when the popup hides
  // mid-gesture (the old window listeners did).
  const onRowDown = useCallback((e: React.PointerEvent, h: Hit) => {
    suppressClick.current = false;
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    const sx = e.clientX, sy = e.clientY;
    let dragging = false;
    el.setPointerCapture(e.pointerId);
    const mv = (ev: PointerEvent) => {
      if (!dragging && Math.hypot(ev.clientX - sx, ev.clientY - sy) > 4) {
        dragging = true;
        suppressClick.current = true; // the click that follows the drag must not open the file
        backend.startDrag([h.path]).catch(fail);
      }
    };
    const stop = () => {
      el.removeEventListener("pointermove", mv);
      el.removeEventListener("pointerup", stop);
    };
    el.addEventListener("pointermove", mv);
    el.addEventListener("pointerup", stop);
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
  const ovGo = useCallback((i: number) => { const key = ovKeyNear(ov.nav, i); if (key) setOselKey(key); }, [ov.nav]);

  const openSemMenu = useCallback(async (path: string, x: number, y: number) => {
    try {
      const r = await backend.showContextMenu(path, x, y);
      if (r === "invoked") { await backend.hide(); return; }
      if (r === "deleted") { flash("Moved to Recycle Bin"); ownRemoved([path]); }
    } catch (e) { fail(e); }
    refocus();
  }, [backend, flash, fail, refocus, ownRemoved]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return; // an IME-confirming Enter/Escape belongs to the composition, not the app
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
          if (ovItem?.launch) { setLaunchMenu({ hit: ovItem.launch, x, y, home: ovItem.section === "mostused" }); return; }
          if (curIdx !== undefined) void openMenu(curIdx, x, y); else void openSemMenu(current.path, x, y);
          return;
        }
        const r = el.getBoundingClientRect();
        void openMenu(sel, r.left + 120, r.top + 4 + sel * rowH - voffRef.current + rowH / 2);
      };
      menuAtRef.current = menuAtSelection;
      const ovStep = (dir: Parameters<typeof ovMove>[2]) => { e.preventDefault(); navigated.current = true; ovGo(ovMove(ov.nav, osel, dir, gridCols)); };
      // The cheat sheet is its own layer: F1 / Ctrl+/ toggles it, Esc closes it first of all.
      if (combos("popup.cheatsheet").some((c) => matchesKey(e, c))) { e.preventDefault(); setCheat((v) => !v); return; }
      if (e.key === "Escape" && guide) { e.preventDefault(); setGuide(false); return; }
      if (e.key === "Escape" && cheat) { e.preventDefault(); setCheat(false); return; }
      // Full window, another page: the page owns the keyboard. Esc leaves Settings for the search —
      // unless a confirm dialog inside it is open: that dialog owns the key until it is answered.
      if (full && page !== "search") {
        if (e.key === "Escape" && page === "settings" && !(e.target as HTMLElement | null)?.closest?.("dialog")) { e.preventDefault(); goPage("search"); refocus(); }
        return;
      }
      // Alt combos (physical-key matching, so Alt+1 works on AZERTY) must preventDefault:
      // WebView2 otherwise beeps or moves focus to the menu bar.
      if (e.altKey && !e.ctrlKey && !e.metaKey) {
        const fi = FILTERS.findIndex((_, i) => matchesKey(e, `Alt+${i + 1}`));
        if (fi >= 0) {
          e.preventDefault();
          const k = parsed.chips.find((c) => c.type === "kind");
          const flt = FILTERS[fi]!;
          if (k && parsed.kind !== flt.id) dropChip(k.key); // a filter key overrides the kind found in the text (like the tab click)
          setFilter(flt.id);
          inputRef.current?.focus();
          return;
        }
        // matchesKey is modifier-exact, so the keymap's promised back-cycle needs its own guard
        if (matchesKey(e, "Alt+S") && !e.shiftKey || matchesKey(e, "Alt+Shift+S")) {
          e.preventDefault();
          cycleSort(e.shiftKey ? -1 : 1);
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
          if (d) { e.preventDefault(); api.pan(d[0]!, d[1]!); return; }
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
          // Space closes Quick Look; it opens it when it cannot be a typed space (empty box, after
          // arrowing, or after clicking a row). Launch rows have no preview: Space is a no-op there.
          // Closing hands the space back to the query, so composing "ab cd" right after works.
          if (ovItem?.launch) break;
          if (pvOpen) { e.preventDefault(); setQuickLook(false); } // Space stays a Quick Look toggle until you type
          else if (current && !showPanelRef.current && !setupOpenRef.current && (e.ctrlKey || navigated.current || query.trim() === "")) {
            e.preventDefault(); setQuickLook(true); recordQuery();
          }
          break;
        case "Enter":
          if (setupOpenRef.current) {
            if ((e.target as HTMLElement | null)?.closest?.("button")) break; // native click
            e.preventDefault(); setupPrimaryRef.current?.(); break;
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
          else if (delArm) disarmDel();
          else if (launchMenu) setLaunchMenu(null);
          else if (pvOpen) { setQuickLook(false); navigated.current = false; }
          else if (expand && !setupOpenRef.current) setExpand(null);
          else if (setupOpenRef.current) { setSetupOpen(false); setSettingsOpen(false); refocus(); } else void backend.hide();
          break;
        case "Backspace": {
          // Remove the last filter chip only at caret 0 with no selection (same path as the chip's ×).
          const atStart = inInput && inp!.selectionStart === 0 && inp!.selectionEnd === 0;
          if (backspaceDropsChip(atStart, hasSelection, parsed.chips.length)) {
            e.preventDefault();
            dropChip(parsed.chips[parsed.chips.length - 1]!.key); // backspaceDropsChip already required a chip
          }
          break;
        }
        case "Tab": {
          if (showPanelRef.current) break; // let focus move into the panel buttons
          e.preventDefault();
          const i = FILTERS.findIndex((f) => f.id === filter);
          setFilter(FILTERS[(i + (e.shiftKey ? FILTERS.length - 1 : 1)) % FILTERS.length]!.id);
          break;
        }
        case "c": case "C":
          // Selected text in the preview outranks every file action: focus usually sits in the
          // search box, so a plain Ctrl+C would copy files while the user sees highlighted text.
          if (e.ctrlKey && !e.shiftKey && !e.altKey) {
            const sel = previewSelectionText(document.getSelection(), document.querySelector(".pv"));
            if (sel) {
              e.preventDefault();
              void backend.copyTextHtml(sel, null).then(() => flash("Copied"), fail);
              break;
            }
          }
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
            // Are-you-sure: the first Delete arms ("press Delete again"), the second recycles.
            if (!delArm) {
              armDel();
              flash(`Press Delete again to move “${current.name}” to the Recycle Bin`, false, 4000);
              break;
            }
            disarmDel();
            if (curIdx !== undefined) void doRecycle(curIdx);
            else backend.recycle([current.path]).then(() => { pushUndo({ kind: "recycle", paths: [current.path] }, "Moved to Recycle Bin"); ownRemoved([current.path]); }).catch(fail);
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
  }, [move, sel, viewH, act, current, curIdx, query, pvOpen, filter, selCount, goPage, setQuickLook, backend, renaming, flash, fail, doRecycle, doUndo, openMenu, openSemMenu, ownRemoved, refreshT1, refocus, openLibrary, overview, ov.nav, osel, ovItem, ovGo, expand, pushUndo, launchAct, armed, delArm, armDel, disarmDel, launchMenu, cheat, guide, openStatus, parsed.chips, dropChip, sort, effSort, recordQuery, full, page, gridCols, setSort, cycleSort, parsed.kind, rowH]);

  useEffect(() => {
    const noMenu = (e: Event) => e.preventDefault();
    window.addEventListener("contextmenu", noMenu);
    return () => window.removeEventListener("contextmenu", noMenu);
  }, []);

  // body is user-select:none, so Chromium KEEPS a text selection alive across clicks on rows and
  // buttons — without this, one selection in the pane would hijack Ctrl+C from then on: every
  // later press copied the stale text instead of the selected file. Clicking outside the pane
  // means the user is done with that text: drop it, and Ctrl+C falls back to file copy. Clicks
  // inside the pane (adjusting the selection, the pane's own copy menu) are left alone.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const sel = document.getSelection();
      const pv = document.querySelector(".pv");
      if (!sel || sel.isCollapsed || !pv) return;
      if (pv.contains(sel.anchorNode) && !pv.contains(e.target as Node)) sel.removeAllRanges();
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
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
      setSetupErr(errText(e));
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
    if (full) goPage("search"); // the setup panel lives on the search page
    setSetupOpen(true);
    models.refreshPlan();
    refreshT1();
  }, [models, refreshT1, full, goPage]);
  const closeSetup = useCallback(() => { setSetupOpen(false); refocus(); }, [refocus]);
  const openSettings = useCallback(() => {
    setSetupOpen(false);
    if (full) { setSettingsOpen(false); goPage("settings"); } else setSettingsOpen(true);
    refreshT1();
  }, [refreshT1, full, goPage]);
  openSettingsRef.current = openSettings;
  const closeSettings = useCallback(() => { setSettingsOpen(false); goPage("search"); refocus(); }, [refocus, goPage]);
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
  const filesText = totalFiles >= 1e6 ? chipCount(totalFiles) : totalFiles.toLocaleString("en-US");
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
        if (drive) return null; // the drive landing carries its own header (the dhome strip)
        const label = FILTERS.find((f) => f.id === filter)?.label ?? "Files";
        const where = drive ? ` on ${drive}:` : "";
        if (sort === "size" && filter === "folders") return `Biggest folders${where}`;
        if (effSort === "date") return `Recent · ${label}${where}`; // newest-first on every tier
        if (sort === "rel") return `Recent · ${label}`; // the unscoped empty tabs (Tier 0 answers newest-first here too)
        return `${label}${where} · ${SORT_LABEL[effSort]}`;
      })()
      : null;
  const sortLabel = SORT_LABEL[effSort];
  const chipLabel = SORTS.find((s) => s.id === effSort)?.label ?? sortLabel; // the chip face is short ("Modified"); prose keeps the full label

  // Virtualised list (Show all files, sorted lists): the rows share the overview's row.
  const rowHandlers: ItemHandlers = useMemo(() => ({
    onSelect: (i) => {
      if (suppressClick.current) { suppressClick.current = false; return; }
      setSel(i);
      // A click selects like arrowing does: Space now means Quick Look, not a typed space. Typing
      // resets it (query change), and closing Quick Look hands the space back to the query too.
      navigated.current = true;
      refocus();
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
      const h = pending ? undefined : data.rows[i];
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
                renaming={renaming && i === sel} y={vl.top + i * rowH - vl.voff} fresh={fresh.has(h.path)} title={why ? `${h.path}\n${why}` : h.path} />,
      );
    }
    return out;
  }, [first, last, vl.voff, vl.top, rowH, pending, data.rows, data.ver, data.recent, parsed.text, nameQ, chipLabels, semMap, sel, backend, renaming, rowHandlers, fresh]); // eslint-disable-line react-hooks/exhaustive-deps -- data.ver counts in-place mutations of the sparse rows array

  const ovHandlers: ItemHandlers = useMemo(() => ({
    onSelect: (i) => {
      if (suppressClick.current) { suppressClick.current = false; return; }
      ovGo(i);
      // A click selects like arrowing does: Space now means Quick Look, not a typed space. Typing
      // resets it (query change), and closing Quick Look hands the space back to the query too.
      navigated.current = true;
      refocus();
    },
    onOpen: (i) => {
      const it = ov.nav.at(i);
      if (it?.calc !== undefined) { void backend.copyTextHtml(it.calc).then(() => backend.hide()).catch(fail); return; }
      // Launch rows never drag, so a stale drag flag must not swallow their click; one launch per id covers click + dblclick.
      if (it?.launch) { if (launchOnce(it.launch.item.id)) void launchAct(it.launch, "open"); return; }
      if (suppressClick.current) return;
      void act("open", it?.hit);
    },
    onMenu: (i, e) => {
      e.preventDefault();
      const it = ov.nav.at(i);
      if (!it || it.calc !== undefined) return;
      ovGo(i);
      if (it.launch) { setLaunchMenu({ hit: it.launch, x: e.clientX, y: e.clientY, home: it.section === "mostused" }); return; }
      if (it.nameIdx !== undefined) void openMenu(it.nameIdx, e.clientX, e.clientY); else void openSemMenu(it.hit.path, e.clientX, e.clientY);
    },
    onDown: (i, e) => { const it = ov.nav.at(i); if (!it) return; if (it.launch) suppressClick.current = false; else onRowDown(e, it.hit); }, // launch rows do not drag
    onRenameCommit: (i, n) => { const idx = ov.nav.at(i)?.nameIdx; if (idx !== undefined) void doRename(idx, n); },
    onRenameCancel: () => { setRenaming(false); refocus(); },
  }), [ov.nav, ovGo, act, launchAct, launchOnce, openMenu, openSemMenu, onRowDown, doRename, refocus, backend, fail]);
  const showList = !overview;
  const waitingSem = overview && sem.query !== parsed.text.trim() && parsed.text.trim().length >= 2 && !!t1 && t1.state !== "unavailable";
  const pvPane = !showPanel && !!pv && !!current;
  const quickLook = pvOpen && pvPane;

  // Result stat in the search bar: "N results · 0.4 ms" (not on home).
  // The opened picture grid's heading already counts its fold ("Other names containing"): not twice.
  const foldCounted = ov.sections.some((x) => x.sparse);
  const statTotal = overview ? ov.sections.reduce((a, x) => a + (foldCounted && x.id === "namepics" ? 0 : x.total), 0) : data.total;
  // The slot is always there (fixed width, see .stat) so the search box never resizes as the count comes and goes.
  const statShown = !isRecent && loaded && !pending;
  const statEl = !showPanel ? (
    <span className={"stat" + (refetching ? " busy" : "")} aria-hidden={!statShown}>
      {statShown && <>
        {statTotal.toLocaleString("en-US")}{data.partial ? "+" : ""} result{statTotal === 1 && !data.partial ? "" : "s"}
        {data.elapsedMs !== null && <> {"·"} <b>{fmtMs(data.elapsedMs)} ms</b></>}
      </>}
    </span>
  ) : null;

  // Free space per drive for the sidebar card (full window only; a minute old is fine).
  const driveKey = status.map((v) => v.letter).join("");
  useEffect(() => {
    if (!driveKey) return;
    let dead = false;
    const load = () => backend.driveSpace(driveKey.split("").filter((l) => /[A-Za-z]/.test(l))).then((s) => { if (!dead) setSpace(s); }).catch(() => { /* the card falls back to file counts */ });
    void load();
    const t = window.setInterval(() => { if (document.visibilityState === "visible") load(); }, 60000);
    return () => { dead = true; window.clearInterval(t); };
  }, [backend, driveKey]);
  // A click on a drive in that card: its folders, biggest first ("what is taking the space").
  const [spaceAt, setSpaceAt] = useState<{ letter: string; n: number } | undefined>(undefined);
  const showDrive = (letter: string) => {
    setSpaceAt((s) => ({ letter, n: (s?.n ?? 0) + 1 }));
    goPage("cleanup");
  };

  // ---- nothing found: the filters that are narrowing the search, each one click to lift ----
  const fixes: { label: string; run: () => void }[] = [];
  if (!isRecent) {
    if (drive) fixes.push({ label: "Search all drives", run: () => setDrive("") });
    if (effFilter !== "all") {
      fixes.push({ label: meaningScope ? "Search names too, not only by meaning" : `Search all types, not only ${FILTERS.find((f) => f.id === effFilter)?.label ?? effFilter}`, run: () => {
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
  // A non-default sort can hide the match you expect: say so, one click back to Best match. With an
  // empty box there is no match to hide, so the note would only be noise.
  const sortNote = !isRecent && query.trim() !== "" && sort !== "rel" ? (
    <div className="sortnote">
      Sorted by <b>{SORT_LABEL[sort]}</b>.
      <button className="fix" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => { setSort("rel"); inputRef.current?.focus(); }}>Use Best match</button>
    </div>
  ) : null;

  // Home suggestions (the empty state is never a dead end): recent searches fill the box, "Try" shows
  // what the query language understands. Most used apps and settings follow below in the Overview.
  const qsug = isRecent ? (
    <div className="qsug">
      {histList.length > 0 && (
        <div className="qsrow">
          <span className="qslab">Recent</span>
          {histList.slice(0, 6).map((q) => (
            <button key={q} className="qs-chip" tabIndex={-1}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => { setQuery(q); histAt.current = -1; inputRef.current?.focus(); }}>
              {q}
            </button>
          ))}
          <button className="qs-clear" tabIndex={-1} aria-label="Clear recent searches"
                  onMouseDown={(e) => e.preventDefault()} onClick={() => { clearHistory(); inputRef.current?.focus(); }}>
            Clear
          </button>
        </div>
      )}
      {!tryHidden && (
      <div className="qsrow">
        <span className="qslab">Try</span>
        {EXAMPLE_QUERIES.map((q) => (
          <button key={q} className="qs-chip ex" tabIndex={-1}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => { setQuery(q); histAt.current = -1; inputRef.current?.focus(); }}>
            {q}
          </button>
        ))}
        <button className="qs-clear" tabIndex={-1} aria-label="Hide suggestions" title="Hide suggestions"
                onMouseDown={(e) => e.preventDefault()} onClick={() => { hideTry(); inputRef.current?.focus(); }}>
          Hide
        </button>
      </div>
      )}
      {loaded && ov.nav.length === 0 && defaultHits && (
        <div className="none qnone"><b>Nothing here yet</b>Start typing to search.</div>
      )}
    </div>
  ) : null;

  const driveMenu = !full || status.length > 3;

  // ---- filter row ----
  const countOf = (f: Filter): number | undefined => {
    if (!nameCounts) return undefined;
    const lc = (k: string) => (drive ? 0 : launch.filter((h) => h.item.kind === k).length);
    switch (f) {
      case "all": return (nameCounts.all ?? 0) + (drive ? 0 : launch.length) + byMeaning.picsByMeaning + byMeaning.docsByMeaning;
      case "apps": return (drive ? 0 : launch.length) + portable.length; // matches launchForFilter("apps")
      case "settings": return lc("setting");
      // A chip counts what its list shows: the index's name matches plus the meaning matches it does not count
      // (the opened Pictures heading is the same sum).
      case "images": return (nameCounts.images ?? 0) + byMeaning.picsByMeaning;
      case "docs": return (nameCounts.docs ?? 0) + byMeaning.docsByMeaning;
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
                    if (k && parsed.kind !== f.id) dropChip(k.key); // a tab click overrides the kind found in the text; the same kind is already in force
                    setFilter(f.id); inputRef.current?.focus();
                  }}>
            {f.label}{n !== undefined && n > 0 && <i className={countsStale ? "stale" : undefined} title={n.toLocaleString("en-US")}>{chipCount(n)}</i>}
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
      {full && !isRecent && overview && (
        <button className={"sortb" + (groupByKind ? " on" : "")} tabIndex={-1} aria-pressed={groupByKind}
                aria-label="Group files by kind"
                title={groupByKind ? "Grouped by kind. Click for one ranked list." : "Group the files into Folders, Documents, Pictures …"}
                onMouseDown={(e) => e.preventDefault()} onClick={() => { toggleGroupKind(); inputRef.current?.focus(); }}>
          Group
        </button>
      )}
      {!isRecent && (
        <div className="dpick" ref={sortCtl.wrapRef}>
          <button className={"sortb" + (effSort !== "rel" ? " custom" : "")} tabIndex={-1} aria-haspopup="listbox" aria-expanded={sortCtl.open}
                  title={effSort !== "rel" ? `Sorted by ${sortLabel}, not Best match` : tooltip("popup.sort")}
                  onMouseDown={(e) => e.preventDefault()} onClick={() => sortCtl.setOpen((o) => !o)}>
            {effSort !== "rel" && <i className="sortdot" aria-hidden="true" />}
            {chipLabel}
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
          </button>
          {sortCtl.open && (
            <div className="dmenu" ref={sortCtl.menuRef} role="listbox" aria-label="Sort">
              {sortOpts.map((s) => (
                <button key={s.id} role="option" aria-selected={effSort === s.id} className={effSort === s.id ? "on" : ""} tabIndex={-1}
                        onMouseDown={(e) => e.preventDefault()}
                        onClick={() => { setSort(s.id); sortCtl.setOpen(false); inputRef.current?.focus(); }}>
                  <span>{SORT_LABEL[s.id]}</span>
                  {effSort === s.id && <i aria-hidden="true">✓</i>}
                </button>
              ))}
            </div>
          )}
        </div>
      )}
      {status.length > 0 && (driveMenu ? (
        <div className="dpick" ref={driveCtl.wrapRef}>
          <button className={"sortb" + (drive ? " on" : "")} tabIndex={-1} aria-haspopup="listbox" aria-expanded={driveCtl.open} title="Limit the search to one drive"
                  onMouseDown={(e) => e.preventDefault()} onClick={() => driveCtl.setOpen((o) => !o)}>
            {drive && <i className="sortdot" aria-hidden="true" />}
            {drive ? `Drive ${drive}` : "All drives"}
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
          </button>
          {driveCtl.open && (
            <div className="dmenu" ref={driveCtl.menuRef} role="listbox" aria-label="Drive">
              {[{ letter: "" }, ...status].map((v) => {
                const sp = space.find((x) => x.letter.toUpperCase() === v.letter.toUpperCase());
                return (
                  <button key={v.letter || "all"} role="option" aria-selected={drive === v.letter} className={drive === v.letter ? "on" : ""} tabIndex={-1}
                          onMouseDown={(e) => e.preventDefault()}
                          onClick={() => { setDrive(v.letter); driveCtl.setOpen(false); inputRef.current?.focus(); }}>
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

  // The drive landing (drive chosen, nothing typed) opens with the scope's own at-a-glance: the
  // drive's free space. Kind filtering stays in the one filter row (frow) above — a second set of
  // kind chips here duplicated it. The list itself is biggest-first, see effSort. Nothing here
  // depends on the USN ring or file recency, so the screen never reshuffles while it sits there.
  const driveSpaceRow = space.find((x) => x.letter.toUpperCase() === drive.toUpperCase());
  const driveHomeEl = driveLanding && loaded && (
    <div className="dhome">
      <div className="dhome-line">
        <b>Drive {drive}:</b>
        {driveSpaceRow && <span>{formatSize(driveSpaceRow.free)} free of {formatSize(driveSpaceRow.total)}</span>}
      </div>
    </div>
  );

  // ---- search box (compact: the popup's top bar; full: the field in the top bar) ----
  const searchInner = (
    <>
      <IMag />
      {parsed.chips.map((c) => (
        <span key={c.key} className="qchip" title="Understood from your search">
          <span>{c.type === "kind" ? "only" : c.type}</span>{c.label}
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
        onPaste={(e) => {
          // a pasted path in any costume (JSON \\, ffmpeg quotes, file:/// URL) searches the file
          const p = pathQueryFromPaste(e.clipboardData.getData("text"));
          if (p === null) return; // ordinary text: default paste
          e.preventDefault();
          const el = e.currentTarget;
          const at = el.selectionStart ?? el.value.length;
          const end = el.selectionEnd ?? at;
          histAt.current = -1;
          setQuery(el.value.slice(0, at) + p + el.value.slice(end));
        }}
        placeholder="Search apps, files and pictures"
        spellCheck={false}
        autoComplete="off"
        autoFocus
        aria-label="Search"
      />
      {/* Clear sits right after the text and Filters stays pinned at the far end; the X keeps its slot (hidden when empty) so nothing shifts. */}
      <button className="clr" tabIndex={-1} aria-label="Clear the search" title="Clear the search" aria-hidden={query === ""}
              style={query === "" ? { visibility: "hidden" } : undefined}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => { setQuery(""); histAt.current = -1; inputRef.current?.focus(); }}>
        <IX size={13} />
      </button>
      <span className="sg-wrap">
        <button className={"sg-btn" + (guide ? " on" : "")} tabIndex={-1} aria-expanded={guide} title="Filters and search syntax"
                onMouseDown={(e) => e.preventDefault()} onClick={() => setGuide((v) => !v)}>
          <IFilter size={14} />Filters
        </button>
        {guide && <SearchGuide query={query} parsed={parsed} onQuery={(q) => { histAt.current = -1; setQuery(q); inputRef.current?.focus(); }} onClose={() => setGuide(false)} />}
      </span>
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
  const canPreview = hasSel && !isCalcSel && !launchSelHit && !!current;
  const actions = !showPanel && hasSel ? (
    <>
      {canPreview && <><button className="act" tabIndex={-1} onMouseDown={keep} onClick={() => setQuickLook(!pvOpen)}>{pvOpen ? "Close preview" : "Preview"} <kbd>Space</kbd></button><span className="vr" /></>}
      <button className="act pri" tabIndex={-1} onMouseDown={keep} onClick={() => enter(false, false)}>{primary} <kbd>{"↵"}</kbd></button>
      {second && <><span className="vr" /><button className="act" tabIndex={-1} onMouseDown={keep} onClick={second.run}>{second.label} {second.keys.map((k) => <kbd key={k}>{k}</kbd>)}</button></>}
      {!isCalcSel && <button className="act" tabIndex={-1} onMouseDown={keep} onClick={() => menuAtRef.current()} title="Rename (F2), Recycle (Del), Copy path, Properties and more">More <kbd>Ctrl</kbd><kbd>K</kbd></button>}
    </>
  ) : null;

  const toastEl = toast ? (
    <span className="toast" role="status">
      {toast.msg}
      {toast.undo && <> {"·"} <button className="undo" tabIndex={-1} onMouseDown={keep} onClick={() => void doUndo()}>Undo</button></>}
    </span>
  ) : null;

  const panels = showPanel && (setupOpen ? (
    <Setup t1={t1} models={models} indexed={status.length ? { files: totalFiles, letters: status.map((v) => v.letter) } : null} onFolders={setFolders} onRemoveFolder={removeFolder} onPause={pauseIndexing} onClose={closeSetup} primaryRef={setupPrimaryRef} estimate={backend.tier1Estimate} />
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
      {/* The shell is a column everywhere so the jobs strip can sit at the very bottom; the sidebar
          and the page columns live in this row. The strip below is on every screen, both modes. */}
      <div className="shell-row">
      {full && <Sidebar page={page} onPage={(p) => { setSpaceAt(undefined); goPage(p); if (p === "search") refocus(); }} status={status} space={space} onDrive={showDrive} />}
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
            {driveHomeEl}
            {full && !isRecent && (
              <div className={"colh" + (dense ? " dense" : "")}>
                <span />
                {COLS.filter((c) => !c.dense || dense).map((c, ci) => {
                  const on = effSort === c.a || effSort === c.b;
                  return (
                    <Fragment key={c.label}>
                    <button tabIndex={-1} className={(on ? "on" : "") + (c.dense ? " ty" : "")} onMouseDown={keep}
                            aria-label={`${c.hint}${on ? `, now ${SORT_LABEL[effSort]}` : ""}`}
                            title={on ? `${SORT_LABEL[effSort]}. Click to reverse.` : c.hint}
                            onClick={() => { setSort(effSort === c.a ? c.b : c.a); inputRef.current?.focus(); }}>
                      {c.label}
                      {on && (
                        <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                          <path d={effSort === c.up ? "m6 15 6-6 6 6" : "m6 9 6 6 6-6"} />
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

            <div className={"list vlist" + (dense ? " dense" : "") + (effSort === "size" || effSort === "size_asc" ? " bysize" : "")} hidden={showPanel || !showList} ref={listRef} onScroll={onListScroll} role="listbox">
              <div className="spacer-v" style={{ height: vl.height }}>{visible}</div>
              {!pending && loaded && data.total === 0 && (
                <div className="none">
                  <b>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</b>
                  {fixes.length > 0 ? "Nothing matches with these filters on." : "Check the spelling, or try fewer words."}
                  {fixEl}
                  {sortNote}
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

            {!showPanel && overview && (pending ? (
              <div className="list ov" /> // a scope switch is being fetched: blank beats the old scope's rows under the new tab
              // "Empty" is a data verdict, never a fold one: a collapsed kind group still reports total > 0 and keeps its
              // reopenable heading, so folds can never produce the dead "No results" screen (folds persist in storage).
            ) : ov.nav.length > 0 || ov.sections.some((s) => s.total > 0) || isRecent ? (
              <Overview onMore={moreTail} onGridCols={setPicCols} sections={ov.sections} sel={osel} renaming={renaming} query={parsed.text} backend={backend} expanded={!!expand} loading={sem.loading || picsBusy}
                        handlers={ovHandlers} onExpand={(id) => { if (id !== "top") setExpand(id as Expand); refocus(); }} onBack={() => { setExpand(null); refocus(); }} onToggleWeak={() => { setWeakOpen((o) => !o); refocus(); }} onToggleFold={(id) => { toggleKindFold(id.slice(2)); refocus(); }} scrollRef={ovRef} armed={armed} lead={qsug} />
            ) : loaded && !waitingSem ? (
              <div className="none">
                {meaningScope && parsed.text.trim().length < 2
                  ? <><b>Search by meaning</b>Describe what the file shows or says, e.g. &ldquo;invoice for the plumber&rdquo;.</>
                  : <><b>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</b>{fixes.length > 0 ? "Nothing matches with these filters on." : "Check the spelling, or try describing what the file is about."}{fixEl}{sortNote}</>}
              </div>
            ) : <div className="list ov" />)}

            {!showPanel && t1Off && !hintOff && query.trim() !== "" && (
              <SemanticHint onSetup={openSetup} onDismiss={() => setHintOff(true)} />
            )}
          </div>

          {/* full window: the preview is always there; compact: only as Quick Look (Space) */}
          {(full || quickLook) && (pvPane ? (
            <Preview backend={backend} hit={pv!.hit} query={parsed.text} snippet={pv!.snippet} page={pv!.page} start={pv!.at ?? undefined} boxes={pv!.boxes} matchPct={pv!.pct} apiRef={previewApi}
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

        {full && page === "search" && !showPanel && (
          <footer className="abar abar-full">
            <KeyHints items={[["↑ ↓", "Move"], ["Space", "Preview"], ["↵", "Open"], ["Ctrl ↵", "Reveal"], ["F2", "Rename"], ["Del", "Recycle"], ["Ctrl K", "More"], ["Tab", "Filter type"], ["Alt S", "Sort"], ["F1", "All keys"]]} />
          </footer>
        )}
        {!full && (
          <footer className="abar">
            <span className="st"><span className={"dot" + (state === "live" ? "" : " warn")} />{statusText}</span>
            {models.running && (
              <button className="dl-link" tabIndex={-1} title="Downloading the search-by-meaning models (one time). Click for details."
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
                    onSetup={openSetup} onClose={closeSettings} onHide={() => void backend.hide()} onLibrary={() => goPage("timeline")}
                    section={settingsSection} onSection={setSettingsSection} onError={fail} embedded chrome={winControls} />
        </div>
      )}
      {full && libSeen && (
        <div className="main-col" hidden={!libPage}>
          <Library backend={backend} embedded tab={libPage ? page : "timeline"} active={libPage}
                   drive={spaceAt} onBack={() => { goPage("search"); refocus(); }} chrome={winControls} />
        </div>
      )}
      </div>

      {/* Work-in-progress strip: always the last thing on screen while any job runs. */}
      <JobsBar backend={backend} volumes={status} t1={t1} models={models} onOpen={openJob} />

      {launchMenu && <LaunchMenu
        menu={{ hit: launchMenu.hit, x: launchMenu.x, y: launchMenu.y }} idx={menuIdx} setIdx={setMenuIdx}
        listRef={menuRef}
        onRun={(mode) => { setLaunchMenu(null); void launchAct(launchMenu.hit, mode); }}
        onRemove={launchMenu.home ? () => removeLaunchMru(launchMenu.hit) : undefined}
        onHide={() => hideLaunch(launchMenu.hit)}
        onClose={() => { setLaunchMenu(null); refocus(); }}
      />}

      <Shortcuts scope="popup" open={cheat} bindings={hotkeyBindings} onClose={() => setCheat(false)} />

      {full && toastEl && <div className="ftoast">{toastEl}</div>}
    </div>
  );
}
