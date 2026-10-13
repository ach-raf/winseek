import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit } from "./types";
import { Preview } from "./Preview";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize } from "./format";
import {
  GRID_PAD, activeSection, gridCols, layoutGrid, moveIndex, railEntries, rowOfItem, scrollIntoView, visibleRows,
} from "./libraryLogic";

const MIN_CELL = 150;
const GAP = 6;
const HEAD_H = 44;
const THUMB = 256;

export interface GridItem {
  path: string;
  name: string;
  size: number | null;
  mtime: number | null;
  /** Semantic score (collections), shown in the tooltip. */
  score?: number;
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
  menu(path: string, x: number, y: number): void;
  /** Recycle Bin (with the undo toast). Never a hard delete. */
  recycle(paths: string[]): Promise<void>;
}

const toHit = (i: GridItem): Hit => ({ name: i.name, path: i.path, is_dir: false, size: i.size, mtime: i.mtime });

function Thumb({ backend, item }: { backend: Backend; item: GridItem }) {
  const src = backend.iconUrl(item.path, false, true, THUMB);
  const [bad, setBad] = useState(false);
  if (!src || bad) return <span className="lg-ph"><FileIcon name={item.name} isDir={false} src={null} /></span>;
  return <img src={src} alt="" draggable={false} decoding="async" onError={() => setBad(true)} />;
}

export function Gallery({ backend, sections, rail, actions, empty }: {
  backend: Backend;
  sections: GallerySection[];
  /** Year / month rail on the right (timeline). */
  rail?: boolean;
  actions: LibActions;
  empty: React.ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [size, setSize] = useState({ w: 900, h: 600 });
  const [sel, setSel] = useState(0);
  const [pvOpen, setPvOpen] = useState(false);
  const [pvItem, setPvItem] = useState<GridItem | null>(null);

  const flat = useMemo(() => sections.flatMap((s) => s.items), [sections]);
  const isEmpty = flat.length === 0;
  const sectionStart = useMemo(() => {
    const out: number[] = [];
    let n = 0;
    for (const s of sections) { out.push(n); n += s.items.length; }
    return out;
  }, [sections]);
  const { cols, cell } = gridCols(size.w, MIN_CELL, GAP);
  const layout = useMemo(
    () => layoutGrid(sections.map((s) => s.items.length), sections.map((s) => s.label), cols, cell, GAP, HEAD_H),
    [sections, cols, cell],
  );
  const railList = useMemo(
    () => (rail ? railEntries(sections.filter((s) => s.year !== undefined && s.month !== undefined) as { year: number; month: number; items: GridItem[] }[]) : []),
    [rail, sections],
  );

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

  // Keep the selected cell on screen.
  const toScroll = useRef(false);
  useEffect(() => {
    const el = box.current;
    if (!el || !toScroll.current) return;
    toScroll.current = false;
    const row = rowOfItem(layout, sel);
    if (!row) return;
    const t = scrollIntoView(row, el.scrollTop, el.clientHeight);
    if (t !== el.scrollTop) el.scrollTop = t;
  }, [sel, layout]);

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
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    switch (e.key) {
      case "ArrowLeft": case "ArrowRight": case "ArrowUp": case "ArrowDown": case "Home": case "End":
        e.preventDefault();
        move(moveIndex(sel, e.key, cols, flat.length));
        break;
      case "PageDown": case "PageUp": {
        e.preventDefault();
        const rows = Math.max(1, Math.floor(size.h / (cell + GAP)) - 1);
        move(Math.max(0, Math.min(flat.length - 1, sel + (e.key === "PageDown" ? 1 : -1) * rows * cols)));
        break;
      }
      case " ": e.preventDefault(); if (cur) setPvOpen((o) => !o); break;
      case "Escape": if (pvOpen) { e.preventDefault(); setPvOpen(false); } break;
      case "Enter": if (cur) { e.preventDefault(); actions.open(cur.path); } break;
      case "Delete": if (cur) { e.preventDefault(); actions.recycle([cur.path]); } break;
      case "F10": if (e.shiftKey && cur) { e.preventDefault(); const r = root.current!.getBoundingClientRect(); actions.menu(cur.path, r.left + 80, r.top + 80); } break;
    }
  };

  const jump = (section: number) => {
    const el = box.current;
    if (!el) return;
    el.scrollTop = layout.sectionTop[section] ?? 0;
    const first = sectionStart[section];
    if (first !== undefined) setSel(first);
    root.current?.focus({ preventScroll: true });
  };

  const activeKey = sections[activeSection(layout.sectionTop, scrollTop)]?.key;
  // The rail follows the scroll position.
  const railRef = useRef<HTMLElement>(null);
  useEffect(() => {
    railRef.current?.querySelector(".lg-rm.on")?.scrollIntoView({ block: "nearest" });
  }, [activeKey]);

  if (isEmpty) return <div className="lg-empty">{empty}</div>;

  const [a, b] = visibleRows(layout.rows, scrollTop, size.h);

  return (
    <div className="lg" ref={root} tabIndex={0} onKeyDown={onKey} role="grid" aria-label="Photos">
      <div className="lg-main">
        <div className="lg-scroll" ref={box} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
          <div className="lg-space" style={{ height: layout.total }}>
            {layout.rows.slice(a, b).map((r) =>
              r.kind === "head" ? (
                <div key={"h" + r.section} className="lg-head" style={{ top: r.top, height: r.h }}>
                  <span>{r.label}</span>
                  <em>{sections[r.section].items.length.toLocaleString("en-US")}</em>
                </div>
              ) : (
                <div key={r.start} className="lg-row" style={{ top: r.top, height: r.h }} role="row">
                  {flat.slice(r.start, r.end).map((it, k) => {
                    const idx = r.start + k;
                    return (
                      <button
                        key={it.path}
                        role="gridcell"
                        aria-selected={idx === sel}
                        className={"lg-cell" + (idx === sel ? " sel" : "")}
                        style={{ left: GRID_PAD + k * (cell + GAP), width: cell, height: cell }}
                        tabIndex={-1}
                        title={`${it.name}${it.score !== undefined ? ` · match ${it.score.toFixed(2)}` : ""}${it.mtime ? ` · ${formatDate(it.mtime)}` : ""}`}
                        onClick={() => { setSel(idx); root.current?.focus({ preventScroll: true }); }}
                        onDoubleClick={() => actions.open(it.path)}
                        onContextMenu={(e) => { e.preventDefault(); setSel(idx); actions.menu(it.path, e.clientX, e.clientY); }}
                      >
                        <Thumb backend={backend} item={it} />
                        <span className="lg-name">{it.name}</span>
                      </button>
                    );
                  })}
                </div>
              ),
            )}
          </div>
        </div>
        <div className="lg-info" aria-live="polite">
          <span>{flat.length.toLocaleString("en-US")} photo{flat.length === 1 ? "" : "s"}</span>
          {cur && (
            <span className="lg-cur" title={cur.path}>
              {cur.name}{cur.size !== null ? ` · ${formatSize(cur.size)}` : ""}{cur.mtime ? ` · ${formatDate(cur.mtime)}` : ""}
            </span>
          )}
          <span className="spacer" />
          <span className="hints">
            <span><kbd>Space</kbd> Preview</span>
            <span><kbd>↵</kbd> Open</span>
            <span><kbd>Ctrl ↵</kbd> Reveal</span>
            <span><kbd>Del</kbd> Recycle</span>
          </span>
        </div>
      </div>
      {pvOpen && pvItem && (
        <div className="lg-pv">
          <Preview backend={backend} hit={toHit(pvItem)} query="" onOpen={() => actions.open(pvItem.path)} onReveal={() => actions.reveal(pvItem.path)} />
        </div>
      )}
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
