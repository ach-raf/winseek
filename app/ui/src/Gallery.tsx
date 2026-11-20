import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox } from "./types";
import { Preview } from "./Preview";
import { FileIcon } from "./FileIcon";
import { ThumbImg } from "./ThumbImg";
import { PicTile, type PicTileEvents } from "./PicTile";
import { noteScroll, warm } from "./thumbQueue";
import { thumbPx, thumbUrl } from "./thumbSize";
import { baseName } from "./overviewLogic";
import { IGrid, IRows } from "./icons";
import { formatDate, formatSize, kindOf, splitDir } from "./format";
import { usePaneFollow, useStoredValue } from "./hooks";
import { activeSection, jumpOffset, parseFoldSet, railEntries, scrubSlot, stepMonth, type GalleryView, type RailSlot } from "./libraryLogic";
import { gridMetrics, gridMove, quickLookKey, rangeForViewport, rowOf, sectionLayout } from "./gridLogic";
import {
  absToEff, buildIndex, effRangeToAbs, effToAbs, rangeCount, rangeHas, rangeToggle, sectionOfEff, type Range,
} from "./timelineLogic";

/** Left / right gutter of the grid (the window's --gut). */
const PAD = 22;
/** Width of the scroller's scrollbar lane (main.tsx measures it): taken off the right gutter so both visible gutters match. */
const sbw = (): number => parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--sbw")) || 0;
const HEAD_H = 50; // 44px sticky heading + 6px air so the first row's selection ring (3px + 2px offset) is not tucked under it
const NO_HEAD = 8; // >= ring 3px + offset 2px, else the first row's ring is clipped by the scroller edge
const BOTTOM = 14;
/** Rows drawn above and below the viewport (a fast scroll never shows a gap). */
const OVERSCAN_ROWS = 2;
/** The scroll position the window is computed from snaps down to this many px: the tiles re-render when the
 *  visible range can change (about twice per row), not on every scroll event. */
const QUANT = 64;
/** Idle prefetch: how far past the viewport (in viewports) the backend renders thumbnails ahead of time. */
const AHEAD = 1.5;
/** Row height of the list view. */
const LIST_H = 32;
/** Thumbnail of a list row (a 22 px box). */
const LIST_PX = thumbPx(22);

const FOLDS_KEY = "winseek.monthfolds";
/** Folded month keys from storage; anything that is not a list of keys is dropped (not cast). */
const readFolds = (raw: string | null): ReadonlySet<string> => {
  try { return parseFoldSet(JSON.parse(raw ?? "null")); } catch { return new Set(); }
};

export interface GridItem {
  path: string;
  /** The file name; left out, it is the last segment of `path` (the timeline passes its index rows as they are
   *  rather than copying every one of them just to add a name). */
  name?: string;
  size: number | null;
  mtime: number | null;
  /** Semantic score (collections), shown in the tooltip. */
  score?: number;
  /** What the search preview shows for a meaning hit (collections pass the collection's hits through). */
  snippet?: string | null;
  page?: number | null;
  /** Audio / video hit: seconds into the clip. */
  at?: number | null;
  boxes?: HitBox[] | null;
}
export interface GallerySection {
  key: string;
  /** Heading above the section; null = none. */
  label: string | null;
  year?: number;
  month?: number;
  items: GridItem[];
}
/** What the library window lets a grid do with a file. */
export interface LibActions {
  open(path: string): void;
  reveal(path: string): void;
  /** Copy the file's full path to the clipboard. */
  copy(path: string): void;
  menu(path: string, x: number, y: number): void;
  /** Recycle Bin (with the undo toast and the are-you-sure gate). Never a hard delete.
   *  Resolves true only when the recycle really happened — a "no" on the confirm keeps the selection. */
  recycle(paths: string[]): Promise<boolean>;
}

/** One more thing a grid can do with the selected file (a collection's "Not in this collection"). */
export interface GridExtra {
  label: string;
  /** Single key, shown as the hint (matched case-insensitively). */
  key: string;
  run(item: GridItem): void;
}

const nameOf = (i: GridItem): string => i.name ?? baseName(i.path);

const toHit = (i: GridItem): Hit => ({ name: nameOf(i), path: i.path, is_dir: false, size: i.size, mtime: i.mtime });

/** Everything that is not a picture / video: its name stays visible on the tile. */
const isDocTile = (name: string) => {
  const k = kindOf(name, false);
  return k !== "image" && k !== "video";
};

const RowThumb = memo(function RowThumb({ backend, path, name, mtime }: { backend: Backend; path: string; name: string; mtime: number | null }) {
  const src = thumbUrl(backend, path, LIST_PX, mtime);
  const ph = <span className="lg-ph"><FileIcon name={name} isDir={false} src={null} /></span>;
  if (!src) return ph;
  return <ThumbImg src={src} bad={ph} path={path} />;
});

/** A grid tile at its arithmetic spot. Primitive props only, so the memo holds across scroll steps. */
const GTile = memo(function GTile({ it, k, left, top, w, h, picked, lead, backend, ev }: {
  it: GridItem; k: number; left: number; top: number; w: number; h: number; picked: boolean; lead: boolean; backend: Backend; ev: PicTileEvents;
}) {
  const name = nameOf(it);
  return (
    <PicTile backend={backend} i={k} path={it.path} name={name} mtime={it.mtime} tileW={w} tileH={h}
             sel={picked} lead={lead} doc={isDocTile(name)} ev={ev}
             style={{ position: "absolute", left, top }}
             title={`${name}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}${it.mtime ? ` · ${formatDate(it.mtime)}` : ""}`} />
  );
});

/** One month of a gallery: what the heading and the layout need (the rows themselves come from the source). */
export interface GallerySec {
  key: string;
  /** Heading above the section; null = none. */
  label: string | null;
  year?: number;
  month?: number;
  count: number;
  bytes: number;
}

/** Where a gallery's rows come from. The timeline's source holds months and fetches rows by absolute index as the
 *  viewport reaches them; a plain list (collections, search results) has them all. A changed `id` is a different
 *  list (selection starts over). */
export interface GallerySource {
  id: unknown;
  secs: GallerySec[];
  total: number;
  /** Changes whenever rows arrive (the grid renders again from the new object). */
  ver: number;
  /** The row at an absolute index, undefined while it is not loaded (the grid draws a skeleton). */
  get(abs: number): GridItem | undefined;
  /** Rows the viewport needs, as flat [start, end) pairs of absolute indices. */
  want(spans: readonly number[]): void;
  /** Count / bytes (and paths when asked) of inclusive absolute index ranges; null when the list changed under the call. */
  pick(ranges: Range[], withPaths: boolean): Promise<{ count: number; bytes: number; paths: string[] } | null>;
}

const NO_SECTIONS: GallerySection[] = [];

/** A fully loaded list as a source. */
function useListSource(sections: GallerySection[], key?: string): GallerySource {
  return useMemo(() => {
    const flat = sections.flatMap((s) => s.items);
    return {
      id: key ?? sections,
      secs: sections.map((s) => ({ key: s.key, label: s.label, year: s.year, month: s.month, count: s.items.length, bytes: s.items.reduce((a, i) => a + (i.size ?? 0), 0) })),
      total: flat.length,
      ver: 0,
      get: (i) => flat[i],
      want: () => {},
      pick: async (ranges, withPaths) => {
        let count = 0, bytes = 0;
        const paths: string[] = [];
        for (const [a, b] of ranges) {
          for (let k = a; k <= b && k < flat.length; k++) {
            count++; bytes += flat[k]!.size ?? 0;
            if (withPaths) paths.push(flat[k]!.path);
          }
        }
        return { count, bytes, paths };
      },
    };
  }, [sections, key]);
}

/** The picture grid of the library window: the shared exact-size tiles (gridMetrics — the same tile as the search
 *  popup's Pictures), sticky month headings, the year / month rail at the grid's right edge, and the preview DOCKED
 *  to the right (popup full mode's `1fr 384px`): always there, with a hint when nothing is selected, so selecting or
 *  opening never moves or resizes the grid. Virtualized: only the rows near the viewport exist.
 *  `view` switches the same sections between tiles and dense list rows (the owner keeps the choice).
 *  Rows come from `sections` (a loaded list) or from `source` (the timeline: months by count, rows by index on demand;
 *  `source={null}` is "still loading"). Indices: "effective" = what is on screen (folded months skipped), "absolute"
 *  = the whole list. `onMore`: the viewport reached the end of what is loaded (search results read more). */
export function Gallery({ backend, sections, listKey, source, rail, actions, empty, extra, noun = "photo", view = "grid", onMore }: {
  backend: Backend;
  sections?: GallerySection[];
  /** With `sections`: what makes it the same list (a growing result list keeps its selection); default the array. */
  listKey?: string;
  source?: GallerySource | null;
  /** Year / month rail on the right (timeline). */
  rail?: boolean;
  actions: LibActions;
  empty: React.ReactNode;
  extra?: GridExtra;
  /** What one item is called in the count ("photo", "item"). */
  noun?: string;
  /** Grid of tiles (default) or dense list rows. */
  view?: GalleryView;
  onMore?: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [size, setSize] = useState({ w: 700, h: 600 });
  const [sel, setSel] = useState(0); // effective index of the lead item
  // Ctrl/Shift batch: absolute index ranges; null = just the `sel` index.
  const [multi, setMulti] = useState<Range[] | null>(null);
  const [anchorAbs, setAnchorAbs] = useState<number | null>(null); // where a Shift+click range starts
  const batch = multi && rangeCount(multi) > 1 ? multi : null;
  // Folded months: the heading stays (with count and size), the items drop. Remembered across sessions.
  const [folds, setFolds] = useStoredValue<ReadonlySet<string>>(FOLDS_KEY, readFolds, (f) => JSON.stringify([...f]));
  const toggleFold = useCallback((key: string) => {
    const n = new Set(folds);
    if (n.has(key)) n.delete(key); else n.add(key);
    setFolds(n);
  }, [setFolds, folds]);

  const listSrc = useListSource(sections ?? NO_SECTIONS, listKey);
  const src = source ?? listSrc;
  const secs = src.secs;
  // A different list (filter, view tab, files gone) starts the selection over; the same list re-read keeps it.
  const [seenId, setSeenId] = useState(src.id);
  if (seenId !== src.id) { setSeenId(src.id); setMulti(null); setAnchorAbs(null); }

  const ix = useMemo(() => buildIndex(secs, folds), [secs, folds]);
  const effTotal = ix.effTotal;
  // Empty = no real items anywhere. Folds do not count: a folded section keeps its heading (so it
  // can be reopened), and folding every section must never read as an empty gallery.
  const isEmpty = src.total === 0;
  const railList = useMemo(
    () => {
      if (!rail) return [];
      // Section summaries only (never rows); `idx` keeps the real section index when some sections have no month.
      const ms: { year: number; month: number; count: number; idx: number }[] = [];
      secs.forEach((s, idx) => { if (s.year !== undefined && s.month !== undefined) ms.push({ year: s.year, month: s.month, count: s.count, idx }); });
      return railEntries(ms);
    },
    [rail, secs],
  );

  // The shared grid: exact-size tiles, as many columns as fit the grid column (the dock and the rail never move).
  const g = useMemo(() => gridMetrics(size.w - PAD - Math.max(8, PAD - sbw()), Infinity, true), [size.w]);
  const { cols, tileW, tileH, rowStride: stride } = g;
  const heads = useMemo(() => secs.map((s) => (s.label ? HEAD_H : NO_HEAD)), [secs]);
  const metrics = useMemo(() => sectionLayout(ix.effCount, heads, g, BOTTOM), [ix, heads, g]);
  // List view: one stacked row per item under the same headings.
  const listMetrics = useMemo(() => sectionLayout(ix.effCount, heads, { cols: 1, rowStride: LIST_H, gap: 0 }, BOTTOM), [ix, heads]);
  const lay = view === "list" ? listMetrics : metrics;

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setSize((p) => (p.w === el.clientWidth && p.h === el.clientHeight ? p : { w: el.clientWidth, h: el.clientHeight }));
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, [isEmpty]);

  useEffect(() => { setSel((s) => Math.max(0, Math.min(s, effTotal - 1))); }, [effTotal]);
  // Becoming empty (or filling again) hands the keyboard to the gallery — but never while the user
  // is typing in a field: the timeline's picture search mounts this gallery mid-typing, and its
  // results flipping empty↔full must not rip focus out of the search box.
  useEffect(() => {
    const ae = document.activeElement;
    if (ae instanceof HTMLElement && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
    root.current?.focus({ preventScroll: true });
  }, [isEmpty]);

  // Moving the selection (keys, rail) scrolls it into view below the sticky heading. A layout change under a
  // stationary selection — window resize, folded month, filter — instead re-anchors the scroll to the item's old
  // viewport spot, so the tile the user is looking at does not jump out of view.
  const toScroll = useRef(false);
  // Which index sat where in the viewport after the last settle. A geometry change re-anchors
  // only when it is still the same item — a click on another tile must never scroll.
  const anchor = useRef<{ sel: number; rel: number } | null>(null);
  // Document-space top and height of the selected item under the current metrics (either view).
  const selBox = useCallback((): { y: number; h: number } | null => {
    if (effTotal === 0) return null;
    const si = sectionOfEff(ix, sel);
    const j = sel - (ix.effStart[si] ?? 0);
    const base = (lay.top[si] ?? 0) + (heads[si] ?? NO_HEAD);
    if (view === "list") return { y: base + j * LIST_H, h: LIST_H };
    return { y: base + rowOf(j, cols) * stride, h: tileH };
  }, [view, effTotal, ix, heads, lay, sel, cols, stride, tileH]);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) { anchor.current = null; return; }
    const sb = selBox();
    if (!sb) { anchor.current = null; return; }
    if (toScroll.current) {
      toScroll.current = false;
      const margin = 8, headroom = HEAD_H + margin;
      let next = el.scrollTop;
      if (sb.y - headroom < next) next = Math.max(0, sb.y - headroom);
      else if (sb.y + sb.h + margin > next + el.clientHeight) next = sb.y + sb.h + margin - el.clientHeight;
      if (next !== el.scrollTop) el.scrollTop = next;
    } else if (anchor.current?.sel === sel) {
      const next = sb.y - anchor.current.rel;
      if (next !== el.scrollTop) el.scrollTop = next; // keep the item's exact viewport spot
    }
    anchor.current = { sel, rel: sb.y - el.scrollTop };
  }, [sel, selBox]);

  const leadAbs = effTotal > 0 ? effToAbs(ix, sel) : 0;
  const cur = effTotal > 0 ? src.get(leadAbs) ?? null : null;
  // The docked preview follows the selection once it settles (arrowing must not load every file); the
  // first pick after an empty pane shows at once. (App's popup pane runs the same hook.)
  const curRef = useRef(cur);
  curRef.current = cur;
  const pvItem = usePaneFollow(cur ? cur.path : null, 120, () => curRef.current);

  // The drawn window, in document px. `scrollTop` is floored to QUANT, so the window reaches one step further down.
  const over = (view === "list" ? LIST_H * 6 : stride * OVERSCAN_ROWS);
  const lo = scrollTop - over, hi = scrollTop + size.h + over + QUANT;
  /** Item range [from, to) of section `si` inside the document band [y0, y1]. */
  const bandOf = useCallback((si: number, y0: number, y1: number): [number, number] => {
    const n = ix.effCount[si] ?? 0;
    const first = (lay.top[si] ?? 0) + (heads[si] ?? NO_HEAD);
    return view === "list"
      ? rangeForViewport(y0, y1 - y0, { cols: 1, rowStride: LIST_H }, n, first, 0)
      : rangeForViewport(y0, y1 - y0, g, n, first, 0);
  }, [ix, lay, heads, view, g]);
  // Which rows the viewport needs, as absolute [start, end) pairs. The selected row is always one (info bar, preview).
  const spansAt = useCallback((y0: number, y1: number, lead: number) => {
    const out: number[] = [];
    if (effTotal === 0) return out;
    ix.effCount.forEach((n, si) => {
      const top = lay.top[si] ?? 0, h = lay.h[si] ?? 0;
      if (n === 0 || top + h < y0 || top > y1) return;
      const [from, to] = bandOf(si, y0, y1);
      if (to > from) out.push((ix.absStart[si] ?? 0) + from, (ix.absStart[si] ?? 0) + to);
    });
    out.push(lead, lead + 1);
    return out;
  }, [effTotal, ix, lay, bandOf]);
  const spans = useMemo(() => spansAt(lo, hi, leadAbs), [spansAt, lo, hi, leadAbs]);
  const spanKey = spans.join(",");
  // After a beat: a fling passes over thousands of rows, only where it stops needs fetching.
  useEffect(() => {
    if (spans.length === 0) return;
    const t = window.setTimeout(() => src.want(spans), 30);
    return () => window.clearTimeout(t);
    // `spans` is its own key (spanKey); `src` changes when rows arrive, which re-asks for what is still missing.
  }, [spanKey, src]); // eslint-disable-line react-hooks/exhaustive-deps

  // The end of what is loaded is in reach: the owner may read more (search results).
  const nearEnd = !isEmpty && hi + size.h >= lay.total;
  useEffect(() => { if (nearEnd) onMore?.(); }, [nearEnd, src.total, onMore]);

  /** One recycle call for the whole batch; the selection clears only when it really happened. */
  const recycleBatch = useCallback((paths: string[]) => {
    void actions.recycle(paths).then((ok) => { if (ok) setMulti(null); });
  }, [actions]);
  const recycleSelection = useCallback(async () => {
    if (batch) {
      const r = await src.pick(batch, true);
      if (r && r.paths.length > 0) recycleBatch(r.paths);
    } else if (cur) recycleBatch([cur.path]);
  }, [batch, src, cur, recycleBatch]);

  // Size of the batch for the bar: asked from the source (the rows are not all here).
  const [bytesOf, setBytesOf] = useState<{ b: Range[]; bytes: number } | null>(null);
  const srcRef = useRef(src);
  srcRef.current = src;
  useEffect(() => {
    if (!batch) return;
    let live = true;
    const t = window.setTimeout(() => {
      void srcRef.current.pick(batch, false).then((r) => { if (live && r) setBytesOf({ b: batch, bytes: r.bytes }); });
    }, 60);
    return () => { live = false; window.clearTimeout(t); };
  }, [batch, src.id]);
  const batchBytes = batch && bytesOf?.b === batch ? bytesOf.bytes : null;
  const batchCount = batch ? rangeCount(batch) : 0;
  const allOn = batch ? batchCount >= effTotal : false;

  /** One click on a tile/row (effective index `k`): plain restarts, Ctrl toggles into the batch, Shift ranges from the
   *  anchor in the shown order. Returns true when the pick was plain (the caller then sets the index and re-focuses
   *  the grid). */
  const pickAt = useCallback((k: number, mod?: { ctrl?: boolean; shift?: boolean; ctx?: boolean }): boolean => {
    const abs = effToAbs(ix, k);
    if (mod?.ctx && (batch ? rangeHas(batch, abs) : k === sel)) return false; // right-click on an already selected item keeps the batch
    // Ctrl/Shift grow the current selection (Explorer): the single lead counts as the batch's seed.
    const lead = effToAbs(ix, sel);
    if (mod?.ctrl) {
      const next = rangeToggle(multi ?? [[lead, lead]], abs);
      setSel(k);
      setMulti(next.length > 0 ? next : null);
      return false;
    }
    if (mod?.shift) {
      const a = anchorAbs ?? lead;
      const ae = absToEff(ix, a);
      const next = ae < 0 ? [[abs, abs]] as Range[] : effRangeToAbs(ix, Math.min(ae, k), Math.max(ae, k));
      setSel(k);
      setMulti(next.length > 0 ? next : null);
      return false;
    }
    setMulti(null);
    setAnchorAbs(abs);
    return true;
  }, [ix, batch, sel, multi, anchorAbs]);

  // Stable handlers for the memoised tiles and rows; they always call the latest closures.
  const live = useRef({ pickAt, actions, itemAt: (k: number): GridItem | undefined => src.get(effToAbs(ix, k)) });
  live.current = { pickAt, actions, itemAt: (k: number) => src.get(effToAbs(ix, k)) };
  const ev = useMemo<PicTileEvents>(() => ({
    onClick: (k, e) => {
      if (live.current.pickAt(k, { ctrl: e.ctrlKey, shift: e.shiftKey })) { setSel(k); root.current?.focus({ preventScroll: true }); }
    },
    onDoubleClick: (k) => { const it = live.current.itemAt(k); if (it) live.current.actions.open(it.path); },
    onContextMenu: (k, e) => {
      e.preventDefault();
      const l = live.current;
      const it = l.itemAt(k);
      if (!it) return;
      if (l.pickAt(k, { ctx: true })) setSel(k);
      l.actions.menu(it.path, e.clientX, e.clientY);
    },
  }), []);

  /** Quick Look (Space): the preview takes the whole body; the grid stays laid out underneath, so nothing re-measures or refetches. */
  const [ql, setQl] = useState(false);
  useEffect(() => { if (effTotal === 0) setQl(false); }, [effTotal]);
  const move = useCallback((i: number) => { toScroll.current = true; setSel(i); setMulti(null); }, []);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey && e.key === "Enter") { if (cur) { e.preventDefault(); actions.reveal(cur.path); } return; }
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "c") { if (cur) { e.preventDefault(); actions.copy(cur.path); } return; }
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const qk = quickLookKey(e.key, ql);
    if (qk) {
      e.preventDefault(); e.stopPropagation();
      if (qk === "toggle") { if (ql || cur) setQl(!ql); } else setQl(false);
      return;
    }
    switch (e.key) {
      case "ArrowLeft": case "ArrowRight": case "ArrowUp": case "ArrowDown": case "Home": case "End": case "PageUp": case "PageDown": {
        e.preventDefault();
        const pageRows = Math.max(1, Math.floor(size.h / (view === "list" ? LIST_H : stride)) - 1);
        if (view === "list" && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
          move(Math.max(0, Math.min(effTotal - 1, sel + (e.key === "ArrowLeft" ? -1 : 1))));
        } else {
          // The one grid keyboard model (gridLogic): rows stay rows, months continue in the same column.
          move(gridMove(sel, e.key, ix.effCount, view === "list" ? 1 : cols, pageRows));
        }
        break;
      }
      case "Enter": if (cur) { e.preventDefault(); actions.open(cur.path); } break;
      case "Delete": {
        if (batch || cur) { e.preventDefault(); void recycleSelection(); }
        break;
      }
      case "[": case "]": {
        // Step to the previous / next month with photos.
        const el = box.current;
        if (!el) break;
        e.preventDefault();
        const from = activeSection(lay.top, el.scrollTop);
        const to = stepMonth(secs.map((s) => s.count), from, e.key === "]" ? 1 : -1);
        if (to !== from) jump(to);
        break;
      }
      default:
        if (extra && cur && e.key.toLowerCase() === extra.key.toLowerCase()) { e.preventDefault(); extra.run(cur); }
        break;
      case "F10": if (e.shiftKey && cur) { e.preventDefault(); const r = root.current!.getBoundingClientRect(); actions.menu(cur.path, r.left + 80, r.top + 80); } break;
    }
  };

  // The month the rail shows as current while a jump / scrub is under way: the scroll position quantises
  // below the target's top, so following it would flash the previous month. Cleared once the scroll settles.
  const [pinKey, setPinKey] = useState<string | null>(null);
  const pinned = useRef(false);
  const unpinTimer = useRef(0);
  const armUnpin = useCallback(() => {
    window.clearTimeout(unpinTimer.current);
    unpinTimer.current = window.setTimeout(() => { pinned.current = false; setPinKey(null); }, 160);
  }, []);
  useEffect(() => () => window.clearTimeout(unpinTimer.current), []);

  /** Instant scroll to a month's heading by arithmetic on the section tops, and the rows there are asked for at once. */
  const goTo = (section: number) => {
    const el = box.current;
    if (!el) return;
    const st = jumpOffset(lay.top, section);
    const key = secs[section]?.key;
    if (key) { pinned.current = true; setPinKey(key); }
    el.scrollTo({ top: st, behavior: "instant" });
    const lead = ix.absStart[section] ?? 0;
    src.want(spansAt(st - over, st + size.h + over + QUANT, lead));
  };
  const goToRef = useRef(goTo);
  useEffect(() => { goToRef.current = goTo; });

  const jump = (section: number) => {
    // A rail click on a folded month unfolds it (that is the "expand on demand" path).
    const sk = secs[section]?.key;
    if (sk && folds.has(sk)) toggleFold(sk);
    goTo(section);
    armUnpin();
    const first = ix.effStart[section];
    if (first !== undefined) { setSel(first); setMulti(null); }
    root.current?.focus({ preventScroll: true });
  };

  // Drag-scrub: pointer down on a month jumps there, dragging along the rail scrubs the whole range (one scroll
  // per frame), a floating label names the month under the pointer. Release commits the selection.
  const [scrubLabel, setScrubLabel] = useState<{ text: string; y: number } | null>(null);
  const scrub = useRef<{ slots: RailSlot[]; secs: number[]; labels: string[]; cur: number; y: number; cy: number; raf: number } | null>(null);
  useEffect(() => () => { if (scrub.current) cancelAnimationFrame(scrub.current.raf); }, []);
  const scrubStep = () => {
    const st = scrub.current;
    if (!st) return;
    st.raf = 0;
    const i = scrubSlot(st.slots, st.y, st.cur);
    if (i < 0) return;
    const body = railRef.current?.parentElement?.getBoundingClientRect();
    if (body) setScrubLabel({ text: st.labels[i]!, y: Math.max(14, Math.min(body.height - 14, st.cy - body.top)) });
    if (i === st.cur) return;
    st.cur = i;
    goToRef.current(st.secs[i]!);
  };
  const onRailDown = (e: React.PointerEvent<HTMLElement>) => {
    const nav = railRef.current;
    if (e.button !== 0 || !nav) return;
    const btns = Array.from(nav.querySelectorAll<HTMLButtonElement>(".lg-rm"));
    const slots = btns.map((b) => ({ top: b.offsetTop, bottom: b.offsetTop + b.offsetHeight, count: Number(b.dataset.c) }));
    const r = nav.getBoundingClientRect();
    scrub.current = {
      slots, secs: btns.map((b) => Number(b.dataset.s)), labels: btns.map((b) => b.dataset.l ?? ""),
      cur: -1, y: e.clientY - r.top + nav.scrollTop, cy: e.clientY, raf: 0,
    };
    e.preventDefault();
    nav.setPointerCapture(e.pointerId);
    pinned.current = true;
    scrubStep();
  };
  const onRailMove = (e: React.PointerEvent<HTMLElement>) => {
    const st = scrub.current, nav = railRef.current;
    if (!st || !nav) return;
    st.y = e.clientY - nav.getBoundingClientRect().top + nav.scrollTop;
    st.cy = e.clientY;
    if (!st.raf) st.raf = requestAnimationFrame(scrubStep);
  };
  const onRailUp = (e: React.PointerEvent<HTMLElement>) => {
    const st = scrub.current;
    if (!st) return;
    scrub.current = null;
    cancelAnimationFrame(st.raf);
    if (railRef.current?.hasPointerCapture(e.pointerId)) railRef.current.releasePointerCapture(e.pointerId);
    setScrubLabel(null);
    const sec = st.cur >= 0 ? st.secs[st.cur] : undefined;
    if (e.type === "pointercancel" || sec === undefined) { armUnpin(); return; }
    jump(sec);
  };

  // Scrolling: one state update per frame, and only when the quantised position moves (see QUANT). The exact
  // position never goes through React; the selection's re-anchor reads it from the element.
  const latest = useRef({ selBox, sel });
  latest.current = { selBox, sel };
  const scrollRaf = useRef(0);
  useEffect(() => () => cancelAnimationFrame(scrollRaf.current), []);
  const onScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    noteScroll(el.scrollTop);
    if (pinned.current && !scrub.current) armUnpin(); // a pinned month is released once the scroll settles
    if (scrollRaf.current) return;
    scrollRaf.current = requestAnimationFrame(() => {
      scrollRaf.current = 0;
      const st = el.scrollTop;
      const q = Math.floor(st / QUANT) * QUANT;
      setScrollTop((p) => (p === q ? p : q));
      // Manual scrolling moves the viewport, not the layout: re-record where the selection
      // sits so a later geometry change (resize) re-anchors from the new spot.
      const l = latest.current;
      const sb = l.selBox();
      if (sb) anchor.current = { sel: l.sel, rel: sb.y - st };
    });
  }, [armUnpin]);

  // Idle prefetch (grid view): once scrolling settles, the thumbnails just past the viewport are rendered into
  // the backend's caches, nearest first; the page decodes nothing until a tile really scrolls in.
  useEffect(() => {
    if (view !== "grid" || isEmpty) return;
    const timer = window.setTimeout(() => {
      const ahead = size.h * AHEAD;
      const y0 = scrollTop - ahead, y1 = scrollTop + size.h + ahead;
      const px = thumbPx(tileW, tileH);
      const below: string[] = [], above: string[] = [];
      ix.effCount.forEach((n, si) => {
        const top = metrics.top[si] ?? 0, h = metrics.h[si] ?? 0;
        if (n === 0 || top + h < y0 || top > y1) return;
        const first = top + (heads[si] ?? NO_HEAD);
        const [from, to] = bandOf(si, y0, y1);
        for (let j = from; j < to; j++) {
          const y = first + rowOf(j, cols) * stride;
          if (y + tileH >= lo && y <= hi) continue; // already drawn
          const it = src.get((ix.absStart[si] ?? 0) + j);
          if (!it) continue; // not fetched yet: the viewport's own fetch comes first
          const u = thumbUrl(backend, it.path, px, it.mtime);
          if (u) (y > scrollTop ? below : above).push(u);
        }
      });
      warm([...below, ...above.reverse()]);
    }, 180);
    return () => window.clearTimeout(timer);
  }, [view, isEmpty, scrollTop, size.h, ix, metrics, heads, src, cols, stride, tileW, tileH, lo, hi, bandOf, backend]);

  // `scrollTop` is floored to QUANT: add the step back so a month jumped to exactly at its top reads as current.
  const activeKey = pinKey ?? secs[activeSection(lay.top, scrollTop + QUANT - 1)]?.key;
  // The rail follows the scroll position.
  const railRef = useRef<HTMLElement>(null);
  useEffect(() => {
    railRef.current?.querySelector(".lg-rm.on")?.scrollIntoView({ block: "nearest" });
  }, [activeKey]);

  if (isEmpty) return <div className="lg-empty">{empty}</div>;

  return (
    <div className={ql ? "lg ql" : "lg"} ref={root} tabIndex={0} data-autofocus onKeyDown={onKey} role="listbox" aria-label="Photos">
      <div className="lg-main">
        <div className="lg-body">
          <div className="lg-scroll" ref={box} onScroll={onScroll}>
            <div className="lg-space" style={{ height: lay.total }}>
              {secs.map((s, si) => {
                const top = lay.top[si] ?? 0;
                const h = lay.h[si] ?? 0;
                const folded = folds.has(s.key);
                const n = ix.effCount[si] ?? 0;
                if (top + h < lo || top > hi) return null;
                if (n === 0 && !folded) return null; // nothing to draw, nothing to reopen
                const head = heads[si] ?? NO_HEAD;
                const absStart = ix.absStart[si] ?? 0, effStart = ix.effStart[si] ?? 0;
                const foldHead = s.label && (
                  <h3 key="h">
                    <button className="lg-fh" aria-expanded={!folded} tabIndex={-1}
                            onClick={() => toggleFold(s.key)}
                            title={folded ? `Show the ${s.count.toLocaleString("en-US")} items` : "Hide these items"}>
                      {folded ? "▸" : "▾"} {s.label}
                      <i>{s.count.toLocaleString("en-US")} items{folded && s.bytes > 0 ? ` · ${formatSize(s.bytes)}` : ""}</i>
                    </button>
                  </h3>
                );
                const [from, to] = n > 0 ? bandOf(si, lo, hi) : [0, 0];
                const rows: React.ReactNode[] = [];
                for (let j = from; j < to; j++) {
                  const abs = absStart + j, k = effStart + j;
                  const it = src.get(abs);
                  const picked = batch ? rangeHas(batch, abs) : k === sel;
                  if (view === "list") {
                    const y = head + j * LIST_H;
                    if (!it) {
                      rows.push(<div key={"s" + abs} className="lg-row sk" aria-hidden style={{ top: y, height: LIST_H }} />);
                      continue;
                    }
                    const { tail } = splitDir(it.path);
                    const nm = nameOf(it);
                    rows.push(
                      <div key={it.path} role="option" aria-selected={picked}
                           className={"lg-row" + (picked ? " sel" : "") + (k === sel ? " lead" : "")}
                           style={{ top: y, height: LIST_H }}
                           title={`${it.path}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}`}
                           onClick={(e) => ev.onClick!(k, e)}
                           onDoubleClick={(e) => ev.onDoubleClick!(k, e)}
                           onContextMenu={(e) => ev.onContextMenu!(k, e)}>
                        <span className="lg-rth"><RowThumb backend={backend} path={it.path} name={nm} mtime={it.mtime} /></span>
                        <span className="lg-rn">{nm}</span>
                        <span className="lg-rd">{tail}</span>
                        <span className="lg-rsp" />
                        <span className="lg-rs">{it.size !== null ? formatSize(it.size) : ""}</span>
                        <span className="lg-rt">{it.mtime ? formatDate(it.mtime) : ""}</span>
                      </div>,
                    );
                    continue;
                  }
                  const left = PAD + (j % cols) * (tileW + g.gap);
                  const y = head + rowOf(j, cols) * stride;
                  if (!it) {
                    rows.push(<div key={"s" + abs} className="pt sk" aria-hidden style={{ position: "absolute", left, top: y, width: tileW, height: tileH }} />);
                    continue;
                  }
                  rows.push(<GTile key={it.path} it={it} k={k} left={left} top={y} w={tileW} h={tileH} picked={picked} lead={k === sel} backend={backend} ev={ev} />);
                }
                return (
                  <section key={s.key} className="mon" style={{ top, height: h }}>
                    {foldHead}
                    {rows}
                  </section>
                );
              })}
            </div>
          </div>
          {rail && railList.length > 0 && (
            <nav className="lg-rail" ref={railRef} aria-label="Jump to month"
                 onPointerDown={onRailDown} onPointerMove={onRailMove} onPointerUp={onRailUp} onPointerCancel={onRailUp}>
              {railList.map((y) => (
                <div key={y.year} className="lg-ry">
                  <div className="lg-yl">{y.year}</div>
                  {y.months.map((m) => {
                    const text = m.count === 0 ? "no photos" : `${m.count.toLocaleString("en-US")} photo${m.count === 1 ? "" : "s"}`;
                    return (
                      <button
                        key={m.section}
                        tabIndex={-1}
                        className={"lg-rm" + (secs[m.section]?.key === activeKey ? " on" : "") + (m.count === 0 ? " empty" : "")}
                        title={`${m.label} ${y.year} · ${text}`}
                        aria-label={`${m.label} ${y.year}, ${text}`}
                        data-s={m.section} data-c={m.count} data-l={`${m.label} ${y.year} · ${text}`}
                        onClick={(e) => { if (e.detail === 0) jump(m.section); }}
                      >
                        {m.label}
                      </button>
                    );
                  })}
                </div>
              ))}
            </nav>
          )}
          {scrubLabel && <div className="lg-scrub" style={{ top: scrubLabel.y }} aria-hidden="true">{scrubLabel.text}</div>}
        </div>
        <div className="lg-info" aria-live="polite">
          <span>{effTotal.toLocaleString("en-US")} {noun}{effTotal === 1 ? "" : "s"}</span>
          {cur && (
            <span className="lg-cur" title={cur.path}>
              {nameOf(cur)}{cur.size !== null ? ` · ${formatSize(cur.size)}` : ""}{cur.mtime ? ` · ${formatDate(cur.mtime)}` : ""}
            </span>
          )}
          <span className="sp" />
          <span className="hints">
            {extra && cur && (
              <button className="lg-act" tabIndex={-1} onClick={() => { extra.run(cur); root.current?.focus({ preventScroll: true }); }}>
                <kbd>{extra.key.toUpperCase()}</kbd> {extra.label}
              </button>
            )}
            <span><kbd>↵</kbd> Open</span>
            {!extra && <span><kbd>Ctrl ↵</kbd> Reveal</span>}
            <span><kbd>Ctrl ⇧ C</kbd> Copy path</span>
            <span><kbd>Shift F10</kbd> More</span>
            <span><kbd>Del</kbd> Recycle</span>
            <span><kbd>Ctrl·Click</kbd> Select many</span>
          </span>
        </div>
        {/* The batch lives in the same bottom selection bar the Clean up suggestions use. */}
        {batch && (
          <div className="selbar" role="region" aria-label="Selection">
            <span>{batchCount.toLocaleString("en-US")} selected</span>
            <button className="btn sm ghost" disabled={allOn} onClick={() => setMulti(effRangeToAbs(ix, 0, effTotal - 1))}>Select all</button>
            <button className="btn sm ghost" onClick={() => { setMulti(null); }}>Clear</button>
            <button className="btn danger" onClick={() => void recycleSelection()}>{batchBytes === null ? "Move to Recycle Bin" : `Move ${formatSize(batchBytes)} to Recycle Bin`}</button>
          </div>
        )}
      </div>
      {pvItem ? (
        <div className="lg-pv">
          {ql && <div className="lg-qlhint" aria-hidden="true"><kbd>Esc</kbd> or <kbd>Space</kbd> to close</div>}
          <Preview backend={backend} hit={toHit(pvItem)} query="" snippet={pvItem.snippet} page={pvItem.page} start={pvItem.at ?? undefined} boxes={pvItem.boxes}
                   onOpen={() => actions.open(pvItem.path)} onReveal={() => actions.reveal(pvItem.path)} onCopyPath={() => actions.copy(pvItem.path)} />
        </div>
      ) : (
        <aside className="lg-pv lg-pv-empty">
          <b>Nothing selected</b>
          <span>Pick a photo or document to see it here.</span>
        </aside>
      )}
    </div>
  );
}

/** The grid/list switch for a gallery (the owner keeps the choice in localStorage). */
export function GalleryViewToggle({ view, onChange }: { view: GalleryView; onChange: (v: GalleryView) => void }) {
  return (
    <div className="dseg gv-seg" role="group" aria-label="How the gallery is shown">
      <button aria-pressed={view === "grid"} className={view === "grid" ? "on" : ""} title="Tiles" onClick={() => onChange("grid")}><IGrid size={14} /></button>
      <button aria-pressed={view === "list"} className={view === "list" ? "on" : ""} title="List with names, sizes and dates" onClick={() => onChange("list")}><IRows size={14} /></button>
    </div>
  );
}
