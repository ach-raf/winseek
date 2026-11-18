import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox } from "./types";
import { Preview } from "./Preview";
import { FileIcon } from "./FileIcon";
import { ThumbImg } from "./ThumbImg";
import { IGrid, IRows } from "./icons";
import { formatDate, formatSize, kindOf, splitDir } from "./format";
import { usePaneFollow, useStoredValue } from "./hooks";
import { activeSection, inBatch, moveMosaic, mosaicMetrics, packMosaic, parseFoldSet, railEntries, rangedBatch, tileBox, toggledBatch, type GalleryView, type SelMod } from "./libraryLogic";

const PAD = 22;
const GAP = 4;
const HEAD_H = 44;
const NO_HEAD = 6;
const BOTTOM = 14;
const OVERSCAN = 320;
/** Row height of the list view. */
const LIST_H = 32;

const FOLDS_KEY = "winseek.monthfolds";
/** Folded month keys from storage; anything that is not a list of keys is dropped (not cast). */
const readFolds = (raw: string | null): ReadonlySet<string> => {
  try { return parseFoldSet(JSON.parse(raw ?? "null")); } catch { return new Set(); }
};

export interface GridItem {
  path: string;
  name: string;
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

const toHit = (i: GridItem): Hit => ({ name: i.name, path: i.path, is_dir: false, size: i.size, mtime: i.mtime });

/** Everything that is not a picture / video: its name stays visible on the tile (a wall of
 *  anonymous document icons is unreadable). */
const isDocTile = (name: string) => {
  const k = kindOf(name, false);
  return k !== "image" && k !== "video";
};

function Thumb({ backend, item, px }: { backend: Backend; item: GridItem; px: number }) {
  const src = backend.iconUrl(item.path, false, true, px);
  const ph = <span className="lg-ph"><FileIcon name={item.name} isDir={false} src={null} /></span>;
  if (!src) return ph;
  return <ThumbImg src={src} bad={ph} />;
}

/** The v4 mosaic: sticky month headings, 8 columns of 104px rows with 2x2 / 2x1 feature tiles. Virtualized.
 *  `preview` = the floating preview pane over the grid's right edge, open by default (collections); Space
 *  collapses and reopens it, and an empty pane shows a hint instead of vanishing. The pane takes no
 *  layout width, so toggling it can never re-tile the mosaic or move the scroll.
 *  `view` switches the same sections between the tile mosaic and dense list rows (the owner keeps the choice). */
export function Gallery({ backend, sections, rail, actions, empty, preview = false, extra, noun = "photo", view = "grid" }: {
  backend: Backend;
  sections: GallerySection[];
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
  const [sel, setSel] = useState(0);
  // Ctrl/Shift batch: real-case paths of every selected item; null = just the `sel` index.
  const [multi, setMulti] = useState<ReadonlySet<string> | null>(null);
  const rangeAnchor = useRef<string | null>(null); // where a Shift+click range starts
  const batch = multi && multi.size > 1 ? multi : null;
  // With a permanent preview pane (collections) it starts open; Space collapses and reopens it.
  const [pvOpen, setPvOpen] = useState(preview);
  // Folded months: the heading stays (with count and size), the items drop. Remembered across sessions.
  const [folds, setFolds] = useStoredValue<ReadonlySet<string>>(FOLDS_KEY, readFolds, (f) => JSON.stringify([...f]));
  const toggleFold = useCallback((key: string) => {
    const n = new Set(folds);
    if (n.has(key)) n.delete(key); else n.add(key);
    setFolds(n);
  }, [setFolds, folds]);

  // A folded section keeps its heading and loses its items; everything below is computed from this.
  const eff = useMemo(
    () => (folds.size === 0 ? sections : sections.map((s) => (folds.has(s.key) ? { ...s, items: [] } : s))),
    [sections, folds],
  );
  // Sizes for the folded headings ("March 2025 ▸ 214 items · 2.1 GB").
  const secBytes = useMemo(() => sections.map((s) => s.items.reduce((a, i) => a + (i.size ?? 0), 0)), [sections]);

  const flat = useMemo(() => eff.flatMap((s) => s.items), [eff]);
  const flatPaths = useMemo(() => flat.map((i) => i.path), [flat]);
  const byPath = useMemo(() => new Map(flat.map((i) => [i.path, i])), [flat]);
  // Empty = no real items anywhere. Folds do not count: a folded section keeps its heading (so it
  // can be reopened), and folding every section must never read as an empty gallery.
  const isEmpty = sections.every((s) => s.items.length === 0);
  const sectionStart = useMemo(() => {
    const out: number[] = [];
    let n = 0;
    for (const s of eff) { out.push(n); n += s.items.length; }
    return out;
  }, [eff]);
  const railList = useMemo(
    () => (rail ? railEntries(eff.filter((s) => s.year !== undefined && s.month !== undefined) as { year: number; month: number; items: GridItem[] }[]) : []),
    [rail, eff],
  );

  // 8 columns when there is room (prototype), fewer next to the preview pane / in narrow windows.
  const inner = Math.max(0, size.w - PAD * 2);
  const cols = inner >= 640 ? 8 : inner >= 460 ? 6 : 4;
  const cell = Math.max(24, (inner - GAP * (cols - 1)) / cols);
  const rowH = Math.max(80, Math.min(132, Math.round(cell * 0.92)));
  const mosaic = useMemo(() => packMosaic(eff.map((s) => s.items.length), cols), [eff, cols]);
  const metrics = useMemo(
    () => mosaicMetrics(mosaic, rowH, GAP, eff.map((s) => (s.label ? HEAD_H : NO_HEAD)), BOTTOM, 28),
    [mosaic, rowH, eff],
  );
  // List view: one stacked row per item under the same headings (no tile packing).
  const listMetrics = useMemo(() => {
    const top: number[] = [], h: number[] = [];
    let y = 0;
    for (const s of eff) {
      const head = s.label ? HEAD_H : NO_HEAD;
      top.push(y);
      const hh = head + s.items.length * LIST_H + (s.items.length > 0 ? BOTTOM : 6);
      h.push(hh);
      y += hh;
    }
    return { top, h, total: y };
  }, [eff]);

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setSize({ w: el.clientWidth, h: el.clientHeight });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, [isEmpty]);

  useEffect(() => { setSel((s) => Math.max(0, Math.min(s, flat.length - 1))); }, [flat.length]);
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
    if (view === "list") {
      let si = -1;
      for (let i = sectionStart.length - 1; i >= 0; i--) if ((sectionStart[i] ?? 0) <= sel) { si = i; break; }
      if (si < 0) return null;
      const head = eff[si]?.label ? HEAD_H : NO_HEAD;
      return { y: (listMetrics.top[si] ?? 0) + head + (sel - (sectionStart[si] ?? 0)) * LIST_H, h: LIST_H };
    }
    const t = mosaic.tiles[sel];
    if (!t) return null;
    const b = tileBox(t, rowH, GAP, metrics.head[t.section] ?? 0);
    return { y: (metrics.top[t.section] ?? 0) + b.y, h: b.h };
  }, [view, sectionStart, eff, listMetrics, sel, mosaic, rowH, metrics]);
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

  const cur = flat[sel] ?? null;
  // The preview follows the selection once it settles (arrowing must not load every file); the
  // first pick after an empty pane shows at once. (App's popup pane runs the same hook.)
  const curRef = useRef(cur);
  curRef.current = cur;
  const pvItem = usePaneFollow(pvOpen && cur ? cur.path : null, 120, () => curRef.current);

  // The lead item is the keyboard/preview selection; everything in the batch is "on" too.
  const isOn = useCallback((p: string) => inBatch(batch, p, flat[sel]?.path), [batch, flat, sel]);
  const batchBytes = useMemo(
    () => (batch ? [...batch].reduce((a, p) => a + (byPath.get(p)?.size ?? 0), 0) : 0),
    [batch, byPath],
  );
  const allOn = useMemo(() => (batch ? flatPaths.length > 0 && flatPaths.every((p) => isOn(p)) : false), [batch, flatPaths, isOn]);
  /** One recycle call for the whole batch; the selection clears only when it really happened. */
  const recycleBatch = useCallback((paths: string[]) => {
    void actions.recycle(paths).then((ok) => { if (ok) setMulti(null); });
  }, [actions]);

  /** One click on a tile/row: plain restarts, Ctrl toggles into the batch, Shift ranges from the
   *  anchor in the shown order. Returns true when the pick was plain (the caller then sets the
   *  index and re-focuses the grid). */
  const pickItem = useCallback((it: GridItem, mod?: SelMod): boolean => {
    if (mod?.ctx && isOn(it.path)) return false; // right-click on an already selected item keeps the batch
    const leadTo = (path: string) => {
      const idx = flatPaths.indexOf(path);
      if (idx >= 0) setSel(idx);
    };
    // Ctrl/Shift grow the current selection (Explorer): the single lead counts as the batch's seed.
    const cur = multi ?? (flat[sel] ? new Set([flat[sel]!.path]) : new Set<string>());
    if (mod?.ctrl) {
      const next = toggledBatch(cur, it.path);
      leadTo(it.path);
      setMulti(next.length > 0 ? new Set(next) : null);
      return false;
    }
    if (mod?.shift) {
      const next = rangedBatch(mod.order ?? flatPaths, rangeAnchor.current, it.path);
      leadTo(it.path);
      setMulti(next.length > 0 ? new Set(next) : null);
      return false;
    }
    setMulti(null);
    rangeAnchor.current = it.path;
    return true;
  }, [multi, sel, flat, isOn, flatPaths]);

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
          if (step === null) move(e.key === "Home" ? 0 : flat.length - 1);
          else move(Math.max(0, Math.min(flat.length - 1, sel + step)));
        } else {
          move(moveMosaic(mosaic, metrics, rowH, GAP, sel, e.key, flat.length));
        }
        break;
      }
      case "PageDown": case "PageUp": {
        e.preventDefault();
        const rows = Math.max(1, Math.floor(size.h / (view === "list" ? LIST_H : rowH + GAP)) - 1);
        move(Math.max(0, Math.min(flat.length - 1, sel + (e.key === "PageDown" ? 1 : -1) * rows * (view === "list" ? 1 : cols))));
        break;
      }
      case " ": e.preventDefault(); if (cur) setPvOpen((o) => !o); break;
      case "Escape": if (pvOpen) { e.preventDefault(); setPvOpen(false); } break;
      case "Enter": if (cur) { e.preventDefault(); actions.open(cur.path); } break;
      case "Delete": {
        const paths = batch ? [...batch] : cur ? [cur.path] : [];
        if (paths.length > 0) { e.preventDefault(); recycleBatch(paths); }
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
    const sk = sections[section]?.key;
    if (sk && folds.has(sk)) toggleFold(sk);
    el.scrollTop = (view === "list" ? listMetrics.top[section] : metrics.top[section]) ?? 0;
    const first = sectionStart[section];
    if (first !== undefined) { setSel(first); setMulti(null); }
    root.current?.focus({ preventScroll: true });
  };

  const activeKey = sections[activeSection(metrics.top, scrollTop)]?.key;
  // The rail follows the scroll position.
  const railRef = useRef<HTMLElement>(null);
  useEffect(() => {
    railRef.current?.querySelector(".lg-rm.on")?.scrollIntoView({ block: "nearest" });
  }, [activeKey]);

  if (isEmpty) return <div className="lg-empty">{empty}</div>;

  const lo = scrollTop - OVERSCAN, hi = scrollTop + size.h + OVERSCAN;

  return (
    <div className="lg" ref={root} tabIndex={0} data-autofocus onKeyDown={onKey} role="listbox" aria-label="Photos">
      <div className="lg-main">
        <div className="lg-scroll" ref={box} onScroll={(e) => {
          setScrollTop(e.currentTarget.scrollTop);
          // Manual scrolling moves the viewport, not the layout: re-record where the selection
          // sits so a later geometry change (preview pane, resize) re-anchors from the new spot.
          const sb = selBox();
          if (sb) anchor.current = { sel, rel: sb.y - e.currentTarget.scrollTop };
        }}>
          <div className="lg-space" style={{ height: view === "list" ? listMetrics.total : metrics.total }}>
            {eff.map((s, si) => {
              const top = (view === "list" ? listMetrics.top[si] : metrics.top[si]) ?? 0;
              const h = (view === "list" ? listMetrics.h[si] : metrics.h[si]) ?? 0;
              const folded = folds.has(s.key);
              if (top + h < lo || top > hi) return null;
              if (s.items.length === 0 && !folded) return null; // nothing to draw, nothing to reopen
              const head = s.label ? HEAD_H : NO_HEAD;
              const orig = sections[si]!; // eff is a 1:1 map of sections, so counts/sizes stay addressable
              const foldHead = s.label && (
                <h3 key="h">
                  <button className="lg-fh" aria-expanded={!folded} tabIndex={-1}
                          onClick={() => toggleFold(s.key)}
                          title={folded ? `Show the ${orig.items.length.toLocaleString("en-US")} items` : "Hide these items"}>
                    {folded ? "▸" : "▾"} {s.label}
                    <i>{orig.items.length.toLocaleString("en-US")} items{folded && secBytes[si]! > 0 ? ` · ${formatSize(secBytes[si]!)}` : ""}</i>
                  </button>
                </h3>
              );
              if (view === "list") {
                const y0 = top + head;
                const r0 = Math.max(0, Math.floor((lo - y0) / LIST_H));
                const r1 = Math.min(s.items.length, Math.ceil((hi - y0) / LIST_H));
                const rows: React.ReactNode[] = [];
                for (let ri = r0; ri < r1; ri++) {
                  const k = (sectionStart[si] ?? 0) + ri;
                  const it = flat[k];
                  if (!it) continue;
                  const { tail } = splitDir(it.path);
                  const picked = isOn(it.path);
                  rows.push(
                    <div key={it.path} role="option" aria-selected={picked}
                         className={"lg-row" + (picked ? " sel" : "") + (k === sel ? " lead" : "")}
                         style={{ top: head + ri * LIST_H, height: LIST_H }}
                         title={`${it.path}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}`}
                         onClick={(ev) => { if (pickItem(it, { ctrl: ev.ctrlKey, shift: ev.shiftKey, order: flatPaths })) { setSel(k); root.current?.focus({ preventScroll: true }); } }}
                         onDoubleClick={() => actions.open(it.path)}
                         onContextMenu={(e) => { e.preventDefault(); if (pickItem(it, { ctx: true, order: flatPaths })) setSel(k); actions.menu(it.path, e.clientX, e.clientY); }}>
                      <span className="lg-rth"><Thumb backend={backend} item={it} px={48} /></span>
                      <span className="lg-rn">{it.name}</span>
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
              const ms = mosaic.sections[si];
              if (!ms) return null; // packed from these same sections, so always present
              const tiles: React.ReactNode[] = [];
              for (let k = ms.first; k < ms.end; k++) {
                const t = mosaic.tiles[k];
                if (!t) continue; // tiles/items line up 1:1 with this range
                const b = tileBox(t, rowH, GAP, head);
                if (top + b.y + b.h < lo || top + b.y > hi) continue;
                const it = flat[k];
                if (!it) continue;
                const picked = isOn(it.path);
                tiles.push(
                  <button
                    key={it.path}
                    role="option"
                    aria-selected={picked}
                    className={"tl" + (t.w === 2 && t.h === 2 ? " w2" : t.w === 2 ? " wide" : "") + (isDocTile(it.name) ? " doc" : "") + (picked ? " sel" : "") + (k === sel ? " lead" : "")}
                    style={{ left: PAD + t.col * (cell + GAP), top: b.y, width: t.w * cell + (t.w - 1) * GAP, height: b.h }}
                    tabIndex={-1}
                    title={`${it.name}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}${it.mtime ? ` · ${formatDate(it.mtime)}` : ""}`}
                    onClick={(ev) => { if (pickItem(it, { ctrl: ev.ctrlKey, shift: ev.shiftKey, order: flatPaths })) { setSel(k); root.current?.focus({ preventScroll: true }); } }}
                    onDoubleClick={() => actions.open(it.path)}
                    onContextMenu={(e) => { e.preventDefault(); if (pickItem(it, { ctx: true, order: flatPaths })) setSel(k); actions.menu(it.path, e.clientX, e.clientY); }}
                  >
                    <Thumb backend={backend} item={it} px={t.w === 2 ? 480 : 256} />
                    <span className="lg-name">{it.name}</span>
                  </button>,
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
          <span>{flat.length.toLocaleString("en-US")} {noun}{flat.length === 1 ? "" : "s"}</span>
          {cur && (
            <span className="lg-cur" title={cur.path}>
              {cur.name}{cur.size !== null ? ` · ${formatSize(cur.size)}` : ""}{cur.mtime ? ` · ${formatDate(cur.mtime)}` : ""}
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
            <span>{batch.size.toLocaleString("en-US")} selected</span>
            <button className="btn sm ghost" disabled={allOn} onClick={() => setMulti(new Set(flatPaths))}>Select all</button>
            <button className="btn sm ghost" onClick={() => { setMulti(null); }}>Clear</button>
            <button className="btn danger" onClick={() => recycleBatch([...batch])}>Move {formatSize(batchBytes)} to Recycle Bin</button>
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
                  className={"lg-rm" + (sections[m.section]?.key === activeKey ? " on" : "")}
                  title={`${sections[m.section]?.label} · ${m.count} photo${m.count === 1 ? "" : "s"}`}
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
