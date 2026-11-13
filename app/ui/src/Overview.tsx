import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { extOf } from "./previewLogic";
import type { Backend, Hit, LaunchHit, LaunchItem } from "./types";
import type { OvItem, OvSection, SectionId } from "./overviewLogic";
import { nicePath, relDate } from "./overviewLogic";
import { launchPath } from "./launchLogic";
import { FileIcon } from "./FileIcon";
import { IChevL } from "./icons";
import { Snippet } from "./Semantic";
import { formatSize, kindOf, matchRanges } from "./format";

/** Highlights `ranges` when given (launcher hits carry exact char ranges), else finds `query`. */
export function Highlight({ text, query, ranges }: { text: string; query: string; ranges?: [number, number][] }) {
  const spans = ranges ?? matchRanges(text, query);
  if (!spans.length) return <>{text}</>;
  const out: React.ReactNode[] = [];
  let at = 0;
  spans.forEach(([s, e], i) => {
    if (s > at) out.push(text.slice(at, s));
    out.push(<mark key={i}>{text.slice(s, e)}</mark>);
    at = e;
  });
  if (at < text.length) out.push(text.slice(at));
  return <>{out}</>;
}

/** Shell icon size asked for: the Top result row draws a 52 px icon. */
const iconSize = (top: boolean) => (top ? 96 : undefined);

/** Icon source for a launcher item — one place for the rows, the tiles and the preview card:
 *  Store apps by AUMID, file-backed apps by their path, the rest fall back to FileIcon's glyph. */
export function launchIconSrc(backend: Backend, item: LaunchItem, big = false): string | null {
  return item.target.t === "aumid" ? backend.appIconUrl(item.target.aumid, big ? 96 : 32)
    : item.target.t === "path" ? backend.iconUrl(item.target.path, false, false, big ? 96 : undefined) : null;
}

/** The preview panel's card for a launcher selection: icon, title, source, the full wrapped
 *  path and the Enter hint — the raw subtitle used to render as one ugly unbreakable line. */
export function LaunchPreview({ backend, l }: { backend: Backend; l: LaunchHit }) {
  const item = l.item;
  const path = launchPath(item);
  return (
    <div className="none app">
      <FileIcon name={item.title} isDir={false} src={launchIconSrc(backend, item, true)} variant={item.kind} />
      <b>{item.title}</b>
      <span className="lsrc">{item.subtitle.split(" · ")[0]}</span>
      {path && <code className="lpath" title={path}>{path}</code>}
      <span className="lgo">Press Enter to {launchVerb(l).toLowerCase()} it.</span>
    </div>
  );
}

export function RenameInput({ initial, isDir, onCommit, onCancel }: {
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

/** What the second Enter would do, shown while a confirm-gated row is armed. */
function confirmVerb(h: LaunchHit): string {
  const name = h.item.target.t === "cmd" ? h.item.target.name : "";
  return name === "restart" ? "restart" : name === "signout" ? "sign out" : name === "empty_bin" ? "empty the Recycle Bin" : "shut down";
}

/** The Enter verb of a launcher item ("Launch ↵", "Run ↵", "Open ↵"). */
export const launchVerb = (h: LaunchHit): string => (h.item.kind === "app" ? "Launch" : h.item.kind === "command" ? "Run" : "Open");

export interface ItemHandlers {
  /** Select (single click). */
  onSelect: (i: number) => void;
  /** Open (double click). */
  onOpen: (i: number) => void;
  onMenu: (i: number, e: React.MouseEvent) => void;
  onDown: (i: number, e: React.MouseEvent) => void;
  onRenameCommit: (i: number, name: string) => void;
  onRenameCancel: () => void;
}

/** The folder a file is in as a breadcrumb; the head shrinks with an ellipsis, the last folder stays whole. */
function PathSub({ path }: { path: string }) {
  const { head, tail } = nicePath(path);
  return <div className="sub path"><span className="head">{head}</span><span className="tail">{tail}</span></div>;
}

/** The compact rows' Type column: "Folder", or the extension in capitals ("File" without one). */
export function typeCell(name: string, isDir: boolean): string {
  if (isDir) return "Folder";
  const e = extOf(name);
  return e ? e.toUpperCase() : "File";
}

/** A file / folder row (also the virtualised list's row): 36 px icon, name + folder, date, size, "Open ↵" when selected. */
export const HitRow = memo(function HitRow({ hit, i, sel, query, backend, h, renaming, top, app, sem, title, y }: {
  hit: Hit; i: number; sel: boolean; query: string; backend: Backend; h: ItemHandlers; renaming: boolean;
  top?: boolean; app?: boolean; sem?: OvItem["sem"]; title?: string;
  /** Virtualised list: the row's offset in px (it is absolutely positioned). */
  y?: number;
}) {
  const kind = kindOf(hit.name, hit.is_dir);
  const thumb = kind === "image" || kind === "video";
  const snippet = sem?.snippet;
  const pct = sem && sem.source !== "text" && sem.source !== "code" ? Math.max(1, Math.min(99, Math.round(sem.score * 100))) : null;
  return (
    <div className={"row" + (top ? " top" : "") + (sel ? " sel" : "")} role="option" aria-selected={sel} style={y === undefined ? undefined : { transform: `translateY(${y}px)` }}
         data-sel={sel ? "1" : undefined} data-i={i} title={title ?? hit.path}
         onClick={renaming ? undefined : () => h.onSelect(i)} onDoubleClick={renaming ? undefined : () => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      <FileIcon name={hit.name} isDir={hit.is_dir} variant={app ? "app" : undefined} thumb={thumb}
                src={backend.iconUrl(hit.path, hit.is_dir, thumb, iconSize(!!top))} />
      <div className="tx">
        <div className="nm">
          {renaming
            ? <RenameInput initial={hit.name} isDir={hit.is_dir} onCommit={(n) => h.onRenameCommit(i, n)} onCancel={h.onRenameCancel} />
            : <Highlight text={hit.name} query={query} />}
          {pct !== null && <span className="pct">{pct}% match</span>}
        </div>
        {snippet
          ? <div className="sub snip" title="Found inside the file"><Snippet text={snippet} /></div>
          : <PathSub path={hit.path} />}
      </div>
      <span className="ty">{typeCell(hit.name, hit.is_dir)}</span>
      <span className="dt">{relDate(hit.mtime)}</span>
      <span className="sz" title={hit.is_dir && hit.size !== null ? "Everything inside this folder" : undefined}>{formatSize(hit.size)}</span>
      <span className="go">{app ? "Launch" : "Open"} <kbd>{"↵"}</kbd></span>
    </div>
  );
});

/** A launcher row: an app, a Windows settings page or a system command. */
const LaunchRow = memo(function LaunchRow({ it, i, sel, query, backend, h, armed }: {
  it: OvItem; i: number; sel: boolean; query: string; backend: Backend; h: ItemHandlers; armed: string | null;
}) {
  const l = it.launch!;
  const item = l.item;
  const top = it.section === "top";
  const isArmed = armed === item.id;
  // Apps and settings have no preview to select for, so one click launches them.
  // Commands keep select-then-open: their open is confirm-gated.
  const oneClick = item.kind === "app" || item.kind === "setting";
  const src = launchIconSrc(backend, item, top);
  return (
    <div className={"row launch" + (top ? " top" : "") + (sel ? " sel" : "") + (isArmed ? " armed" : "")}
         role="option" aria-selected={sel} data-sel={sel ? "1" : undefined} data-i={i} title={item.subtitle}
         onClick={() => (oneClick ? h.onOpen(i) : h.onSelect(i))} onDoubleClick={() => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      <FileIcon name={item.title} isDir={false} src={src} variant={item.kind} />
      <div className="tx">
        <div className="nm"><Highlight text={item.title} query={query} ranges={l.ranges} /></div>
        <div className="sub">{isArmed ? `Press Enter again to ${confirmVerb(l)}` : item.subtitle}</div>
      </div>
      <span className="dt" />
      <span className="sz" />
      <span className="go">{launchVerb(l)} <kbd>{"↵"}</kbd></span>
    </div>
  );
});

/** Most used: an app tile. */
const AppTile = memo(function AppTile({ it, i, sel, backend, h }: { it: OvItem; i: number; sel: boolean; backend: Backend; h: ItemHandlers }) {
  const item = it.launch!.item;
  const src = launchIconSrc(backend, item, true);
  return (
    <div className={"atile" + (sel ? " sel" : "")} role="option" aria-selected={sel} aria-label={item.title}
         data-sel={sel ? "1" : undefined} data-i={i} title={item.subtitle}
         onClick={() => h.onOpen(i)} onDoubleClick={() => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      <FileIcon name={item.title} isDir={false} src={src} variant="app" />
      <span>{item.title}</span>
    </div>
  );
});

/** Photos: a 4:3 tile; the file name shows on hover / selection. */
const Tile = memo(function Tile({ it, i, sel, query, backend, h }: { it: OvItem; i: number; sel: boolean; query: string; backend: Backend; h: ItemHandlers }) {
  const src = backend.iconUrl(it.hit.path, false, true, 256);
  return (
    <div className={"tile" + (sel ? " sel" : "")} role="option" aria-selected={sel} aria-label={it.hit.name}
         data-sel={sel ? "1" : undefined} data-i={i} title={it.hit.path}
         onClick={() => h.onSelect(i)} onDoubleClick={() => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      {src ? <img src={src} alt="" loading="lazy" decoding="async" draggable={false} /> : <FileIcon name={it.hit.name} isDir={false} src={null} />}
      <span className="cap"><Highlight text={it.hit.name} query={query} /></span>
    </div>
  );
});

/** The calculator result card (replaces the Top result row). */
function CalcCard({ it, i, sel, expr, h }: { it: OvItem; i: number; sel: boolean; expr: string; h: ItemHandlers }) {
  return (
    <div className={"calc" + (sel ? " sel" : "")} role="option" aria-selected={sel} data-sel={sel ? "1" : undefined} data-i={i}
         onClick={() => h.onOpen(i)} onMouseDown={(e) => h.onDown(i, e)} onContextMenu={(e) => h.onMenu(i, e)}>
      <span className="ex">{expr} =</span>
      <span className="v">{it.calc}</span>
      <span className="k">Copy <kbd>{"↵"}</kbd></span>
    </div>
  );
}

/** Lists at least this long are windowed: only the rows near the viewport are in the page. */
const WIN_MIN = 80;
/** Drawn before the first measurement (enough to see a second row of any grid). */
const WIN_FIRST = 48;
/** Rows kept drawn above and below the viewport, so a fast scroll never shows a gap. */
const WIN_OVERSCAN = 4;

/**
 * A list or tile grid of any length that costs what is on screen: the rows near the viewport are
 * drawn, the rest is two spacers of the same height, so the scrollbar still spans everything.
 * The row height and the column count are read from the drawn items (the CSS stays in charge of
 * the layout); every item must carry `data-i` and all rows must be equally tall.
 */
function Windowed({ scrollRef, count, className, render }: {
  scrollRef: React.RefObject<HTMLDivElement | null>; count: number; className: string;
  render: (from: number, to: number) => ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [g, setG] = useState({ cols: 1, stride: 0, gap: 0, top: 0, viewH: 0 });
  const [scrollTop, setScrollTop] = useState(0);
  const on = count >= WIN_MIN;

  const measure = useCallback(() => {
    const el = box.current, sc = scrollRef.current;
    if (!el || !sc) return;
    const els = el.querySelectorAll<HTMLElement>(":scope > [data-i]");
    if (!els.length) return;
    const y0 = els[0].getBoundingClientRect().top;
    let cols = 1;
    while (cols < els.length && Math.abs(els[cols].getBoundingClientRect().top - y0) < 0.5) cols++;
    if (cols >= els.length) return; // a single row is drawn: no second one to take the height from
    const stride = els[cols].getBoundingClientRect().top - y0; // fractional on purpose: it is multiplied by thousands of rows
    const cs = getComputedStyle(el);
    const gap = parseFloat(cs.rowGap) || 0;
    const top = el.getBoundingClientRect().top + (parseFloat(cs.paddingTop) || 0) - sc.getBoundingClientRect().top + sc.scrollTop;
    const viewH = sc.clientHeight;
    setG((p) => (p.cols === cols && Math.abs(p.stride - stride) < 0.01 && p.gap === gap && Math.abs(p.top - top) < 0.5 && p.viewH === viewH
      ? p : { cols, stride, gap, top, viewH }));
  }, [scrollRef]);

  useLayoutEffect(() => { if (on) measure(); }, [on, measure, className]);
  useEffect(() => {
    const sc = scrollRef.current;
    if (!sc || !on) return;
    const onScroll = () => setScrollTop(sc.scrollTop);
    onScroll();
    sc.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(measure);
    ro.observe(sc);
    if (box.current) ro.observe(box.current);
    return () => { sc.removeEventListener("scroll", onScroll); ro.disconnect(); };
  }, [scrollRef, on, measure]);

  let from = 0, to = count, before = 0, after = 0;
  if (on && !g.stride) to = Math.min(count, WIN_FIRST);
  else if (on) {
    const rows = Math.ceil(count / g.cols);
    const y = scrollTop - g.top;
    const a = Math.max(0, Math.min(rows - 1, Math.floor(y / g.stride) - WIN_OVERSCAN));
    const b = Math.max(a, Math.min(rows - 1, Math.ceil((y + g.viewH) / g.stride) + WIN_OVERSCAN));
    from = a * g.cols;
    to = Math.min(count, (b + 1) * g.cols);
    // A spacer stands for whole rows; the gap after it is the grid's own.
    before = a * g.stride - g.gap;
    after = (rows - 1 - b) * g.stride - g.gap;
  }
  return (
    <div ref={box} className={className + (on ? " win" : "")}>
      {before > 0 && <div className="win-pad" style={{ height: before }} />}
      {render(from, to)}
      {after > 0 && <div className="win-pad" style={{ height: after }} />}
    </div>
  );
}

function SectionHead({ s, expanded, loading, onBack, onExpand }: { s: OvSection; expanded: boolean; loading: boolean; onBack: () => void; onExpand: (id: SectionId) => void }): ReactNode {
  return (
    <div className="sh">
      {expanded ? (
        <button className="back" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onBack} aria-label={`Back to all results from ${s.title}`}>
          <IChevL size={14} /><span>{s.title}</span><i>{s.total.toLocaleString("en-US")}</i>
          {loading && <i className="busy" role="status">finding more</i>}
        </button>
      ) : (
        <span>{s.title}</span>
      )}
      {(s.note || s.more) && (
        <small>
          {s.note}
          {s.more && (
            <button className="more" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => onExpand(s.id)}>
              Show all{!s.open && s.total > s.items.length ? ` ${s.total.toLocaleString("en-US")}` : ""}
            </button>
          )}
        </small>
      )}
    </div>
  );
}

export function Overview({ sections, sel, renaming, query, backend, expanded, loading, handlers, onExpand, onBack, scrollRef, armed }: {
  sections: OvSection[]; sel: number; renaming: boolean; query: string; backend: Backend; expanded: boolean;
  /** "Show all" is still reading pages: the list keeps growing. */
  loading?: boolean;
  handlers: ItemHandlers; onExpand: (id: SectionId) => void; onBack: () => void;
  scrollRef: React.RefObject<HTMLDivElement | null>; armed: string | null;
}) {
  let base = 0;
  return (
    <div className="list ov" ref={scrollRef} role="listbox" aria-label="Results">
      {sections.map((s) => {
        const start = base;
        base += s.items.length;
        const first = s.items[0];
        if (s.id === "top" && first?.calc !== undefined) {
          return <CalcCard key={s.id} it={first} i={start} sel={start === sel} expr={query.trim()} h={handlers} />;
        }
        return (
          <section key={s.id} className={"ov-sec s-" + s.id} aria-label={s.title}>
            <SectionHead s={s} expanded={expanded} loading={expanded && !!loading} onBack={onBack} onExpand={onExpand} />
            {s.id === "images" ? (
              <Windowed scrollRef={scrollRef} count={s.items.length} className="grid photos" render={(from, to) =>
                s.items.slice(from, to).map((it, j) => <Tile key={it.key} it={it} i={start + from + j} sel={start + from + j === sel} query={query} backend={backend} h={handlers} />)} />
            ) : s.id === "mostused" ? (
              <div className="grid apps">
                {s.items.map((it, j) => <AppTile key={it.key} it={it} i={start + j} sel={start + j === sel} backend={backend} h={handlers} />)}
              </div>
            ) : (
              <Windowed scrollRef={scrollRef} count={s.items.length} className="rows" render={(from, to) =>
                s.items.slice(from, to).map((it, j0) => {
                  const j = from + j0;
                  return it.launch
                    ? <LaunchRow key={it.key} it={it} i={start + j} sel={start + j === sel} query={query} backend={backend} h={handlers} armed={armed} />
                    : <HitRow key={it.key} hit={it.hit} i={start + j} sel={start + j === sel} query={query} backend={backend} h={handlers}
                              renaming={renaming && start + j === sel} top={s.id === "top"} app={s.id === "apps"} sem={it.sem} />;
                })} />
            )}
          </section>
        );
      })}
    </div>
  );
}
