import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox } from "./types";
import { Preview } from "./Preview";
import { FileIcon } from "./FileIcon";
import { ThumbImg } from "./ThumbImg";
import { noteScroll, warm } from "./thumbQueue";
import { thumbPx } from "./thumbSize";
import { baseName } from "./overviewLogic";
import { IGrid, IRows } from "./icons";
import { formatDate, formatSize, kindOf, splitDir } from "./format";
import { usePaneFollow, useStoredValue } from "./hooks";
import { activeSection, mosaicMetrics, parseFoldSet, railEntries, type GalleryView } from "./libraryLogic";
import {
  absToEff, buildIndex, effRangeToAbs, effToAbs, moveBy, packTemplate, rangeCount, rangeHas, rangeToggle, sectionOfEff, templateBox, tilesInBand, type Range,
} from "./timelineLogic";

const PAD = 22;
const GAP = 4;
const HEAD_H = 44;
const NO_HEAD = 6;
const BOTTOM = 14;
const OVERSCAN = 320;
/** The scroll position the window is computed from snaps down to this many px: the tiles re-render when the
 *  visible range can change (about once per row), not on every scroll event. Rows are 80-132 px. */
const QUANT = 96;
/** Idle prefetch: how far past the viewport (in viewports) the backend renders thumbnails ahead of time. */
const AHEAD = 1.5;
/** Row height of the list view. */
const LIST_H = 32;
/** Thumbnail of a list row (a 48 px box). */
const LIST_PX = thumbPx(48);

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

/** Everything that is not a picture / video: its name stays visible on the tile (a wall of
 *  anonymous document icons is unreadable). */
const isDocTile = (name: string) => {
  const k = kindOf(name, false);
  return k !== "image" && k !== "video";
};

const Thumb = memo(function Thumb({ backend, path, name, px }: { backend: Backend; path: string; name: string; px: number }) {
  const src = backend.iconUrl(path, false, true, px);
  const ph = <span className="lg-ph"><FileIcon name={name} isDir={false} src={null} /></span>;
  if (!src) return ph;
  return <ThumbImg src={src} bad={ph} />;
});

/** What a tile needs from the grid: stable functions that read the grid's latest state, so a tile only
 *  re-renders when its own props change (a scroll step re-renders the visible window, not every tile in it). */
interface TileApi {
  click(it: GridItem, k: number, ev: React.MouseEvent): void;
  open(it: GridItem): void;
  menu(it: GridItem, k: number, ev: React.MouseEvent): void;
}

const GalleryTile = memo(function GalleryTile({ it, k, left, top, width, height, w2, wide, picked, lead, px, title, backend, api }: {
  it: GridItem; k: number; left: number; top: number; width: number; height: number;
  w2: boolean; wide: boolean; picked: boolean; lead: boolean; px: number; title: string; backend: Backend; api: TileApi;
}) {
  const name = nameOf(it);
  return (
    <button
      role="option"
      aria-selected={picked}
      className={"tl" + (w2 ? " w2" : wide ? " wide" : "") + (isDocTile(name) ? " doc" : "") + (picked ? " sel" : "") + (lead ? " lead" : "")}
      style={{ left, top, width, height }}
      tabIndex={-1}
      title={title}
      onClick={(ev) => api.click(it, k, ev)}
      onDoubleClick={() => api.open(it)}
      onContextMenu={(e) => api.menu(it, k, e)}
    >
      <Thumb backend={backend} path={it.path} name={name} px={px} />
      <span className="lg-name">{name}</span>
    </button>
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
 *  viewport reaches them; a plain list (collections) has them all. A changed `id` is a different list (selection starts over). */
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
function useListSource(sections: GallerySection[]): GallerySource {
  return useMemo(() => {
    const flat = sections.flatMap((s) => s.items);
    return {
      id: sections,
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
  }, [sections]);
}

/** The v4 mosaic: sticky month headings, 8 columns of 104px rows with 2x2 / 2x1 feature tiles. Virtualized.
 *  `preview` = the floating preview pane over the grid's right edge, open by default (collections); Space
 *  collapses and reopens it, and an empty pane shows a hint instead of vanishing. The pane takes no
 *  layout width, so toggling it can never re-tile the mosaic or move the scroll.
 *  `view` switches the same sections between the tile mosaic and dense list rows (the owner keeps the choice).
 *  Rows come from `sections` (a loaded list) or from `source` (the timeline: months by count, rows by index on demand;
 *  `source={null}` is "still loading"). The layout is per month, from one shared tile template: nothing is
 *  built per photo. Indices: "effective" = what is on screen (folded months skipped), "absolute" = the whole list. */
export function Gallery({ backend, sections, source, rail, actions, empty, preview = false, extra, noun = "photo", view = "grid" }: {
  backend: Backend;
  sections?: GallerySection[];
  source?: GallerySource | null;
  /** Year / month rail on the right (timeline). */
  rail?: boolean;
  actions: LibActions;
  empty: React.ReactNode;
  preview?: boolean;
  extra?: GridExtra;
  /** What one item is called in the count ("photo", "item"). */
  noun?: string;
  /** Grid of tiles (default) or dense list rows. */
  view?: GalleryView;
}) {
  const box = useRef<HTMLDivElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [size, setSize] = useState({ w: 900, h: 600 });
  const [sel, setSel] = useState(0); // effective index of the lead item
  // Ctrl/Shift batch: absolute index ranges; null = just the `sel` index.
  const [multi, setMulti] = useState<Range[] | null>(null);
  const [anchorAbs, setAnchorAbs] = useState<number | null>(null); // where a Shift+click range starts
  const batch = multi && rangeCount(multi) > 1 ? multi : null;
  // With a permanent preview pane (collections) it starts open; Space collapses and reopens it.
  const [pvOpen, setPvOpen] = useState(preview);
  // Folded months: the heading stays (with count and size), the items drop. Remembered across sessions.
  const [folds, setFolds] = useStoredValue<ReadonlySet<string>>(FOLDS_KEY, readFolds, (f) => JSON.stringify([...f]));
  const toggleFold = useCallback((key: string) => {
    const n = new Set(folds);
    if (n.has(key)) n.delete(key); else n.add(key);
    setFolds(n);
  }, [setFolds, folds]);

  const listSrc = useListSource(sections ?? NO_SECTIONS);
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
    () => (rail ? railEntries(secs.filter((s) => s.year !== undefined && s.month !== undefined) as { year: number; month: number; count: number }[]) : []),
    [rail, secs],
  );

  // 8 columns when there is room (prototype), fewer next to the preview pane / in narrow windows.
  const inner = Math.max(0, size.w - PAD * 2);
  const cols = inner >= 640 ? 8 : inner >= 460 ? 6 : 4;
  const cell = Math.max(24, (inner - GAP * (cols - 1)) / cols);
  const rowH = Math.max(80, Math.min(132, Math.round(cell * 0.92)));
  const heads = useMemo(() => secs.map((s) => (s.label ? HEAD_H : NO_HEAD)), [secs]);
  const maxN = useMemo(() => ix.effCount.reduce((a, n) => Math.max(a, n), 0), [ix]);
  const tpl = useMemo(() => packTemplate(maxN, cols), [maxN, cols]);
  const metrics = useMemo(
    () => mosaicMetrics({ tiles: [], cols, sections: ix.effCount.map((n) => ({ first: 0, end: n, rows: tpl.rowsAt[n] ?? 0 })) }, rowH, GAP, heads, BOTTOM, 28),
    [ix, tpl, cols, rowH, heads],
  );
  // List view: one stacked row per item under the same headings (no tile packing).
  const listMetrics = useMemo(() => {
    const top: number[] = [], h: number[] = [];
    let y = 0;
    ix.effCount.forEach((n, si) => {
      top.push(y);
      const hh = (heads[si] ?? NO_HEAD) + n * LIST_H + (n > 0 ? BOTTOM : 6);
      h.push(hh);
      y += hh;
    });
    return { top, h, total: y };
  }, [ix, heads]);

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setSize({ w: el.clientWidth, h: el.clientHeight });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, [isEmpty]);

  useEffect(() => { setSel((s) => Math.max(0, Math.min(s, effTotal - 1))); }, [effTotal]);
  useEffect(() => { root.current?.focus({ preventScroll: true }); }, [isEmpty]);

  // Moving the selection (keys, rail) scrolls it into view below the sticky heading. A layout
  // change under a stationary selection — preview pane open/close, window resize, folded month,
  // filter — instead re-anchors the scroll to the item's old viewport spot: toggling the preview
  // moves the pane, never the results, so the tile the user is looking at does not jump out of
  // view (and does not "come back" on close either).
  const toScroll = useRef(false);
  // Which index sat where in the viewport after the last settle. A geometry change re-anchors
  // only when it is still the same item — a click on another tile must never scroll.
  const anchor = useRef<{ sel: number; rel: number } | null>(null);
  // Document-space top and height of the selected item under the current metrics (either view).
  const selBox = useCallback((): { y: number; h: number } | null => {
    if (effTotal === 0) return null;
    const si = sectionOfEff(ix, sel);
    const j = sel - (ix.effStart[si] ?? 0);
    if (view === "list") {
      return { y: (listMetrics.top[si] ?? 0) + (heads[si] ?? NO_HEAD) + j * LIST_H, h: LIST_H };
    }
    const b = templateBox(tpl, j, rowH, GAP, metrics.head[si] ?? 0);
    return { y: (metrics.top[si] ?? 0) + b.y, h: b.h };
  }, [view, effTotal, ix, heads, listMetrics, sel, tpl, rowH, metrics]);
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
  // The preview follows the selection once it settles (arrowing must not load every file); the
  // first pick after an empty pane shows at once. (App's popup pane runs the same hook.)
  const curRef = useRef(cur);
  curRef.current = cur;
  const pvItem = usePaneFollow(pvOpen && cur ? cur.path : null, 120, () => curRef.current);

  // Which rows the viewport needs, as absolute [start, end) pairs. The selected row is always one (info bar, preview).
  const lo = scrollTop - OVERSCAN, hi = scrollTop + size.h + OVERSCAN + QUANT; // `scrollTop` is floored to QUANT, so the window reaches one step further down
  const spans = useMemo(() => {
    const out: number[] = [];
    if (effTotal === 0) return out;
    ix.effCount.forEach((n, si) => {
      const top = (view === "list" ? listMetrics.top[si] : metrics.top[si]) ?? 0;
      const h = (view === "list" ? listMetrics.h[si] : metrics.h[si]) ?? 0;
      if (n === 0 || top + h < lo || top > hi) return;
      const head = heads[si] ?? NO_HEAD;
      let from: number, to: number;
      if (view === "list") {
        from = Math.max(0, Math.floor((lo - top - head) / LIST_H));
        to = Math.min(n, Math.ceil((hi - top - head) / LIST_H));
      } else {
        [from, to] = tilesInBand(tpl, n, rowH, GAP, head, lo - top, hi - top);
      }
      if (to > from) out.push((ix.absStart[si] ?? 0) + from, (ix.absStart[si] ?? 0) + to);
    });
    out.push(leadAbs, leadAbs + 1);
    return out;
  }, [effTotal, ix, view, listMetrics, metrics, heads, tpl, rowH, lo, hi, leadAbs]);
  const spanKey = spans.join(",");
  // After a beat: a fling passes over thousands of rows, only where it stops needs fetching.
  useEffect(() => {
    if (spans.length === 0) return;
    const t = window.setTimeout(() => src.want(spans), 30);
    return () => window.clearTimeout(t);
    // `spans` is its own key (spanKey); `src` changes when rows arrive, which re-asks for what is still missing.
  }, [spanKey, src]); // eslint-disable-line react-hooks/exhaustive-deps

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

  // Stable handlers for the memoised tiles; they always call the latest closures.
  const live = useRef({ pickAt, actions });
  live.current = { pickAt, actions };
  const tileApi = useMemo<TileApi>(() => ({
    click: (_it, k, ev) => {
      if (live.current.pickAt(k, { ctrl: ev.ctrlKey, shift: ev.shiftKey })) { setSel(k); root.current?.focus({ preventScroll: true }); }
    },
    open: (it) => live.current.actions.open(it.path),
    menu: (it, k, e) => {
      e.preventDefault();
      const l = live.current;
      if (l.pickAt(k, { ctx: true })) setSel(k);
      l.actions.menu(it.path, e.clientX, e.clientY);
    },
  }), []);

  const move = useCallback((i: number) => { toScroll.current = true; setSel(i); setMulti(null); }, []);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey && e.key === "Enter") { if (cur) { e.preventDefault(); actions.reveal(cur.path); } return; }
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "c") { if (cur) { e.preventDefault(); actions.copy(cur.path); } return; }
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    switch (e.key) {
      case "ArrowLeft": case "ArrowRight": case "ArrowUp": case "ArrowDown": case "Home": case "End": {
        e.preventDefault();
        if (view === "list") {
          const step = e.key === "ArrowUp" || e.key === "ArrowLeft" ? -1 : e.key === "ArrowDown" || e.key === "ArrowRight" ? 1 : null;
          if (step === null) move(e.key === "Home" ? 0 : effTotal - 1);
          else move(Math.max(0, Math.min(effTotal - 1, sel + step)));
        } else {
          const boxAt = (k: number) => {
            const si = sectionOfEff(ix, k);
            const b = templateBox(tpl, k - (ix.effStart[si] ?? 0), rowH, GAP, metrics.head[si] ?? 0);
            return { ...b, y: (metrics.top[si] ?? 0) + b.y };
          };
          move(moveBy(sel, e.key, effTotal, boxAt));
        }
        break;
      }
      case "PageDown": case "PageUp": {
        e.preventDefault();
        const rows = Math.max(1, Math.floor(size.h / (view === "list" ? LIST_H : rowH + GAP)) - 1);
        move(Math.max(0, Math.min(effTotal - 1, sel + (e.key === "PageDown" ? 1 : -1) * rows * (view === "list" ? 1 : cols))));
        break;
      }
      case " ": e.preventDefault(); if (cur) setPvOpen((o) => !o); break;
      case "Escape": if (pvOpen) { e.preventDefault(); setPvOpen(false); } break;
      case "Enter": if (cur) { e.preventDefault(); actions.open(cur.path); } break;
      case "Delete": {
        if (batch || cur) { e.preventDefault(); void recycleSelection(); }
        break;
      }
      default:
        if (extra && cur && e.key.toLowerCase() === extra.key.toLowerCase()) { e.preventDefault(); extra.run(cur); }
        break;
      case "F10": if (e.shiftKey && cur) { e.preventDefault(); const r = root.current!.getBoundingClientRect(); actions.menu(cur.path, r.left + 80, r.top + 80); } break;
    }
  };

  const jump = (section: number) => {
    const el = box.current;
    if (!el) return;
    // A rail click on a folded month unfolds it (that is the "expand on demand" path).
    const sk = secs[section]?.key;
    if (sk && folds.has(sk)) toggleFold(sk);
    el.scrollTop = (view === "list" ? listMetrics.top[section] : metrics.top[section]) ?? 0;
    const first = ix.effStart[section];
    if (first !== undefined) { setSel(first); setMulti(null); }
    root.current?.focus({ preventScroll: true });
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
    if (scrollRaf.current) return;
    scrollRaf.current = requestAnimationFrame(() => {
      scrollRaf.current = 0;
      const st = el.scrollTop;
      const q = Math.floor(st / QUANT) * QUANT;
      setScrollTop((p) => (p === q ? p : q));
      // Manual scrolling moves the viewport, not the layout: re-record where the selection
      // sits so a later geometry change (preview pane, resize) re-anchors from the new spot.
      const l = latest.current;
      const sb = l.selBox();
      if (sb) anchor.current = { sel: l.sel, rel: sb.y - st };
    });
  }, []);

  // Idle prefetch (grid view): once scrolling settles, the thumbnails just past the viewport are rendered into
  // the backend's caches, nearest first; the page decodes nothing until a tile really scrolls in.
  useEffect(() => {
    if (view !== "grid" || isEmpty) return;
    const timer = window.setTimeout(() => {
      const ahead = size.h * AHEAD;
      const y0 = scrollTop - ahead, y1 = scrollTop + size.h + ahead;
      const onLo = scrollTop - OVERSCAN, onHi = scrollTop + size.h + OVERSCAN + QUANT;
      const below: string[] = [], above: string[] = [];
      ix.effCount.forEach((n, si) => {
        const top = metrics.top[si] ?? 0, h = metrics.h[si] ?? 0;
        if (n === 0 || top + h < y0 || top > y1) return;
        const head = heads[si] ?? NO_HEAD;
        const [from, to] = tilesInBand(tpl, n, rowH, GAP, head, y0 - top, y1 - top);
        for (let j = from; j < to; j++) {
          const b = templateBox(tpl, j, rowH, GAP, head);
          const y = top + b.y;
          if (y > y1 || y + b.h < y0 || (y + b.h >= onLo && y <= onHi)) continue; // too far, or already drawn
          const it = src.get((ix.absStart[si] ?? 0) + j);
          if (!it) continue; // not fetched yet: the viewport's own fetch comes first
          const u = backend.iconUrl(it.path, false, true, thumbPx(b.w * cell + (b.w - 1) * GAP, b.h));
          if (u) (y > scrollTop ? below : above).push(u);
        }
      });
      warm([...below, ...above.reverse()]);
    }, 180);
    return () => window.clearTimeout(timer);
  }, [view, isEmpty, scrollTop, size.h, ix, metrics, tpl, heads, src, rowH, cell, backend]);

  const activeKey = secs[activeSection(metrics.top, scrollTop)]?.key;
  // The rail follows the scroll position.
  const railRef = useRef<HTMLElement>(null);
  useEffect(() => {
    railRef.current?.querySelector(".lg-rm.on")?.scrollIntoView({ block: "nearest" });
  }, [activeKey]);

  if (isEmpty) return <div className="lg-empty">{empty}</div>;

  return (
    <div className="lg" ref={root} tabIndex={0} data-autofocus onKeyDown={onKey} role="listbox" aria-label="Photos">
      <div className="lg-main">
        <div className="lg-scroll" ref={box} onScroll={onScroll}>
          <div className="lg-space" style={{ height: view === "list" ? listMetrics.total : metrics.total }}>
            {secs.map((s, si) => {
              const top = (view === "list" ? listMetrics.top[si] : metrics.top[si]) ?? 0;
              const h = (view === "list" ? listMetrics.h[si] : metrics.h[si]) ?? 0;
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
              if (view === "list") {
                const y0 = top + head;
                const r0 = Math.max(0, Math.floor((lo - y0) / LIST_H));
                const r1 = Math.min(n, Math.ceil((hi - y0) / LIST_H));
                const rows: React.ReactNode[] = [];
                for (let ri = r0; ri < r1; ri++) {
                  const k = effStart + ri;
                  const it = src.get(absStart + ri);
                  if (!it) {
                    rows.push(<div key={"s" + (absStart + ri)} className="lg-row sk" aria-hidden style={{ top: head + ri * LIST_H, height: LIST_H }} />);
                    continue;
                  }
                  const { tail } = splitDir(it.path);
                  const picked = batch ? rangeHas(batch, absStart + ri) : k === sel;
                  const nm = nameOf(it);
                  rows.push(
                    <div key={it.path} role="option" aria-selected={picked}
                         className={"lg-row" + (picked ? " sel" : "") + (k === sel ? " lead" : "")}
                         style={{ top: head + ri * LIST_H, height: LIST_H }}
                         title={`${it.path}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}`}
                         onClick={(ev) => tileApi.click(it, k, ev)}
                         onDoubleClick={() => actions.open(it.path)}
                         onContextMenu={(e) => tileApi.menu(it, k, e)}>
                      <span className="lg-rth"><Thumb backend={backend} path={it.path} name={nm} px={LIST_PX} /></span>
                      <span className="lg-rn">{nm}</span>
                      <span className="lg-rd">{tail}</span>
                      <span className="lg-rsp" />
                      <span className="lg-rs">{it.size !== null ? formatSize(it.size) : ""}</span>
                      <span className="lg-rt">{it.mtime ? formatDate(it.mtime) : ""}</span>
                    </div>,
                  );
                }
                return (
                  <section key={s.key} className="mon" style={{ top, height: h }}>
                    {foldHead}
                    {rows}
                  </section>
                );
              }
              const tiles: React.ReactNode[] = [];
              const [from, to] = n > 0 ? tilesInBand(tpl, n, rowH, GAP, head, lo - top, hi - top) : [0, 0];
              for (let j = from; j < to; j++) {
                const b = templateBox(tpl, j, rowH, GAP, head);
                if (top + b.y + b.h < lo || top + b.y > hi) continue;
                const abs = absStart + j, k = effStart + j;
                const left = PAD + b.col * (cell + GAP);
                const width = b.w * cell + (b.w - 1) * GAP;
                const it = src.get(abs);
                if (!it) {
                  tiles.push(<div key={"s" + abs} className="tl sk" aria-hidden style={{ left, top: b.y, width, height: b.h }} />);
                  continue;
                }
                tiles.push(
                  <GalleryTile
                    key={it.path}
                    it={it}
                    k={k}
                    left={left}
                    top={b.y}
                    width={width}
                    height={b.h}
                    w2={b.w === 2 && b.hs === 2}
                    wide={b.w === 2 && b.hs !== 2}
                    picked={batch ? rangeHas(batch, abs) : k === sel}
                    lead={k === sel}
                    px={thumbPx(width, b.h)}
                    title={`${nameOf(it)}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}${it.mtime ? ` · ${formatDate(it.mtime)}` : ""}`}
                    backend={backend}
                    api={tileApi}
                  />,
                );
              }
              return (
                <section key={s.key} className="mon" style={{ top, height: h }}>
                  {foldHead}
                  {tiles}
                </section>
              );
            })}
          </div>
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
            <span><kbd>Space</kbd> {pvOpen ? "Close preview" : "Preview"}</span>
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
      {pvOpen && (preview || pvItem) && (pvItem ? (
        <div className={"lg-pv" + (rail ? " lg-pv-r" : "")}>
          <Preview backend={backend} hit={toHit(pvItem)} query="" snippet={pvItem.snippet} page={pvItem.page} start={pvItem.at ?? undefined} boxes={pvItem.boxes}
                   onOpen={() => actions.open(pvItem.path)} onReveal={() => actions.reveal(pvItem.path)} onCopyPath={() => actions.copy(pvItem.path)} />
        </div>
      ) : (
        <aside className={"lg-pv lg-pv-empty" + (rail ? " lg-pv-r" : "")}>
          <b>Nothing selected</b>
          <span>Pick a photo or document to see it here.</span>
        </aside>
      ))}
      {rail && railList.length > 0 && (
        <nav className="lg-rail" ref={railRef} aria-label="Jump to month">
          {railList.map((y) => (
            <div key={y.year} className="lg-ry">
              <div className="lg-yl">{y.year}</div>
              {y.months.map((m) => (
                <button
                  key={m.section}
                  tabIndex={-1}
                  className={"lg-rm" + (secs[m.section]?.key === activeKey ? " on" : "")}
                  title={`${secs[m.section]?.label} · ${m.count} photo${m.count === 1 ? "" : "s"}`}
                  onClick={() => jump(m.section)}
                >
                  {m.label}
                </button>
              ))}
            </div>
          ))}
        </nav>
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
