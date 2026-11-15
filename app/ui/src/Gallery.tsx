import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox } from "./types";
import { Preview } from "./Preview";
import { FileIcon } from "./FileIcon";
import { ThumbImg } from "./ThumbImg";
import { IGrid, IRows } from "./icons";
import { formatDate, formatSize, kindOf, splitDir } from "./format";
import { activeSection, moveMosaic, mosaicMetrics, packMosaic, parseFoldSet, railEntries, tileBox, type GalleryView } from "./libraryLogic";

const PAD = 22;
const GAP = 4;
const HEAD_H = 44;
const NO_HEAD = 6;
const BOTTOM = 14;
const OVERSCAN = 320;
/** Row height of the list view. */
const LIST_H = 32;

const FOLDS_KEY = "winseek.monthfolds";
const readFolds = (): ReadonlySet<string> => {
  try { return parseFoldSet(JSON.parse(localStorage.getItem(FOLDS_KEY) ?? "null")); } catch { return new Set(); }
};
const saveFolds = (folds: ReadonlySet<string>) => {
  try { localStorage.setItem(FOLDS_KEY, JSON.stringify([...folds])); } catch { /* storage off: folds just are not remembered */ }
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
  /** Recycle Bin (with the undo toast). Never a hard delete. */
  recycle(paths: string[]): Promise<void>;
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
 *  `preview` = the search-style preview pane on the right, open by default (collections); Space collapses
 *  and reopens it, and an empty pane shows a hint instead of vanishing.
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
  // With a permanent preview pane (collections) it starts open; Space collapses and reopens it.
  const [pvOpen, setPvOpen] = useState(preview);
  const [pvItem, setPvItem] = useState<GridItem | null>(null);
  // Folded months: the heading stays (with count and size), the items drop. Remembered across sessions.
  const [folds, setFolds] = useState<ReadonlySet<string>>(readFolds);
  const toggleFold = useCallback((key: string) => {
    setFolds((s) => {
      const n = new Set(s);
      if (n.has(key)) n.delete(key); else n.add(key);
      saveFolds(n);
      return n;
    });
  }, []);

  // A folded section keeps its heading and loses its items; everything below is computed from this.
  const eff = useMemo(
    () => (folds.size === 0 ? sections : sections.map((s) => (folds.has(s.key) ? { ...s, items: [] } : s))),
    [sections, folds],
  );
  // Sizes for the folded headings ("March 2025 ▸ 214 items · 2.1 GB").
  const secBytes = useMemo(() => sections.map((s) => s.items.reduce((a, i) => a + (i.size ?? 0), 0)), [sections]);

  const flat = useMemo(() => eff.flatMap((s) => s.items), [eff]);
  const isEmpty = flat.length === 0;
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

  // Keep the selected item on screen (below the sticky heading), in either view.
  const toScroll = useRef(false);
  useEffect(() => {
    const el = box.current;
    if (!el || !toScroll.current) return;
    toScroll.current = false;
    let y: number, ih: number;
    if (view === "list") {
      let si = -1;
      for (let i = sectionStart.length - 1; i >= 0; i--) if ((sectionStart[i] ?? 0) <= sel) { si = i; break; }
      if (si < 0) return;
      const head = eff[si]?.label ? HEAD_H : NO_HEAD;
      y = (listMetrics.top[si] ?? 0) + head + (sel - (sectionStart[si] ?? 0)) * LIST_H;
      ih = LIST_H;
    } else {
      const t = mosaic.tiles[sel];
      if (!t) return;
      const b = tileBox(t, rowH, GAP, metrics.head[t.section] ?? 0);
      y = (metrics.top[t.section] ?? 0) + b.y;
      ih = b.h;
    }
    const margin = 8, headroom = HEAD_H + margin;
    let next = el.scrollTop;
    if (y - headroom < next) next = Math.max(0, y - headroom);
    else if (y + ih + margin > next + el.clientHeight) next = y + ih + margin - el.clientHeight;
    if (next !== el.scrollTop) el.scrollTop = next;
  }, [sel, view, mosaic, metrics, listMetrics, sectionStart, eff]);

  const cur = flat[sel] ?? null;
  // The preview follows the selection once it settles (arrowing must not load every file).
  const curRef = useRef(cur);
  curRef.current = cur;
  const curPath = cur?.path;
  useEffect(() => {
    if (!pvOpen) { setPvItem(null); return; }
    const t = window.setTimeout(() => setPvItem(curRef.current), pvItem ? 120 : 0);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pvOpen, curPath]);

  const move = useCallback((i: number) => { toScroll.current = true; setSel(i); }, []);

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
      case "Delete": if (cur) { e.preventDefault(); actions.recycle([cur.path]); } break;
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
    if (first !== undefined) setSel(first);
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
        <div className="lg-scroll" ref={box} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
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
                  rows.push(
                    <div key={it.path} role="option" aria-selected={k === sel}
                         className={"lg-row" + (k === sel ? " sel" : "")}
                         style={{ top: head + ri * LIST_H, height: LIST_H }}
                         title={`${it.path}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}`}
                         onClick={() => { setSel(k); root.current?.focus({ preventScroll: true }); }}
                         onDoubleClick={() => actions.open(it.path)}
                         onContextMenu={(e) => { e.preventDefault(); setSel(k); actions.menu(it.path, e.clientX, e.clientY); }}>
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
                tiles.push(
                  <button
                    key={it.path}
                    role="option"
                    aria-selected={k === sel}
                    className={"tl" + (t.w === 2 && t.h === 2 ? " w2" : t.w === 2 ? " wide" : "") + (isDocTile(it.name) ? " doc" : "") + (k === sel ? " sel" : "")}
                    style={{ left: PAD + t.col * (cell + GAP), top: b.y, width: t.w * cell + (t.w - 1) * GAP, height: b.h }}
                    tabIndex={-1}
                    title={`${it.name}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}${it.mtime ? ` · ${formatDate(it.mtime)}` : ""}`}
                    onClick={() => { setSel(k); root.current?.focus({ preventScroll: true }); }}
                    onDoubleClick={() => actions.open(it.path)}
                    onContextMenu={(e) => { e.preventDefault(); setSel(k); actions.menu(it.path, e.clientX, e.clientY); }}
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
            <span><kbd>Space</kbd> Preview</span>
            <span><kbd>↵</kbd> Open</span>
            {!extra && <span><kbd>Ctrl ↵</kbd> Reveal</span>}
            <span><kbd>Del</kbd> Recycle</span>
          </span>
        </div>
      </div>
      {pvOpen && (preview || pvItem) && (pvItem ? (
        <div className="lg-pv">
          <Preview backend={backend} hit={toHit(pvItem)} query="" snippet={pvItem.snippet} page={pvItem.page} start={pvItem.at ?? undefined} boxes={pvItem.boxes}
                   onOpen={() => actions.open(pvItem.path)} onReveal={() => actions.reveal(pvItem.path)} onCopyPath={() => actions.copy(pvItem.path)} />
        </div>
      ) : (
        <aside className="lg-pv lg-pv-empty">
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
