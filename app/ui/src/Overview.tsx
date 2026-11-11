import { memo, useEffect, useRef } from "react";
import type { Backend, LaunchHit } from "./types";
import type { OvItem, OvSection, SectionId } from "./overviewLogic";
import { FileIcon } from "./FileIcon";
import { Snippet } from "./Semantic";
import { formatDate, kindOf, matchRanges, splitDir } from "./format";

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

const KIND_LABEL: Record<string, string> = {
  folder: "Folder", image: "Image", doc: "Document", video: "Video", audio: "Audio", archive: "Archive", code: "Code", app: "App", file: "File",
};

const LAUNCH_CHIP: Record<string, string> = { app: "App", setting: "Settings", command: "Command" };

/** What the second Enter would do, shown while a confirm-gated row is armed. */
function confirmVerb(h: LaunchHit): string {
  const name = h.item.target.t === "cmd" ? h.item.target.name : "";
  return name === "restart" ? "restart" : name === "signout" ? "sign out" : name === "empty_bin" ? "empty the Recycle Bin" : "shut down";
}

/** Minimal stroke icons for the system command rows (settings use the font glyph instead). */
function CommandIcon({ name }: { name: string }) {
  const p = {
    lock: <><rect x="5" y="11" width="14" height="9" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></>,
    sleep: <><path d="M12 3a9 9 0 1 0 9 9 7 7 0 0 1-9-9z" /></>,
    restart: <><path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 3v5h-5" /></>,
    shutdown: <><path d="M12 3v9" /><path d="M18.4 6.6a9 9 0 1 1-12.8 0" /></>,
    signout: <><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /><path d="m16 17 5-5-5-5" /><path d="M21 12H9" /></>,
    empty_bin: <><path d="M3 6h18" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" /></>,
    open_bin: <><path d="M3 6h18" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /><path d="M5 6v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V6" /></>,
    winseek_settings: <><circle cx="12" cy="12" r="3" /><path d="M19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.4-2.3 1a7 7 0 0 0-2-1.2L14.2 3h-4l-.4 2.5a7 7 0 0 0-2 1.2l-2.3-1-2 3.4 2 1.5A7 7 0 0 0 5 12c0 .4 0 .8.1 1.2l-2 1.5 2 3.4 2.3-1a7 7 0 0 0 2 1.2l.4 2.4h4l.4-2.4a7 7 0 0 0 2-1.2l2.3 1 2-3.4-2-1.5c.1-.4.1-.8.1-1.2z" /></>,
    winseek_library: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 9h18" /><path d="m8 13 2.5 2.5L8 18" /></>,
    calc: <><path d="M5 7h14" /><path d="m8 12 8 6M16 12l-8 6" /></>,
  }[name] ?? <><circle cx="12" cy="12" r="9" /><path d="M12 8v4" /><path d="M12 16h.01" /></>;
  return (
    <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {p}
    </svg>
  );
}

/** The folder a file is in, shortened to its last two parts ("…\Projects\winseek"). */
function where(path: string): string {
  const dir = path.slice(0, path.lastIndexOf("\\"));
  const parts = dir.split("\\").filter(Boolean);
  return parts.length <= 2 ? dir : "…\\" + parts.slice(-2).join("\\");
}

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

const Row = memo(function Row({ it, i, sel, query, backend, h, renaming, armed }: {
  it: OvItem; i: number; sel: boolean; query: string; backend: Backend; h: ItemHandlers; renaming: boolean; armed: string | null;
}) {
  const { hit } = it;
  const l = it.launch;
  if (l) {
    const item = l.item;
    const isArmed = armed === item.id;
    // Apps and settings have no preview to select for, so one click launches them.
    // Commands (and the calc Top Hit) keep select-then-open: their open is confirm-gated or copies.
    const oneClick = it.calc === undefined && (item.kind === "app" || item.kind === "setting");
    const icon = item.target.t === "aumid"
      ? backend.appIconUrl(item.target.aumid, it.section === "top" ? 96 : 32)
      : item.target.t === "path" ? backend.iconUrl(item.target.path, false, false, it.section === "top" ? 96 : undefined) : null;
    return (
      <div className={"ov-row launch" + (it.section === "top" ? " top" : "") + (sel ? " selected" : "") + (isArmed ? " armed" : "")}
           role="option" aria-selected={sel} data-sel={sel ? "1" : undefined} data-i={i} title={item.subtitle}
           onClick={() => (oneClick ? h.onOpen(i) : h.onSelect(i))} onDoubleClick={() => h.onOpen(i)}
           onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
        {item.icon
          ? <span className="glyph" aria-hidden>{item.icon}</span>
          : item.target.t === "cmd"
            ? <span className="glyph svg" aria-hidden><CommandIcon name={item.target.name} /></span>
            : <FileIcon name={hit.name} isDir={false} src={icon} thumb={false} />}
        <div className="ov-meta">
          <div className="ov-name"><Highlight text={item.title} query={query} ranges={l.ranges} /></div>
          <div className="ov-sub">{isArmed ? `Press Enter again to ${confirmVerb(l)}` : item.subtitle}</div>
        </div>
        <span className="kind-chip">{it.calc !== undefined ? "Calculator" : LAUNCH_CHIP[item.kind]}</span>
      </div>
    );
  }
  const kind = kindOf(hit.name, hit.is_dir);
  const thumb = kind === "image" || kind === "video";
  const top = it.section === "top";
  const snippet = it.sem?.snippet;
  const { head, tail } = splitDir(hit.path);
  return (
    <div className={"ov-row" + (top ? " top" : "") + (sel ? " selected" : "")} role="option" aria-selected={sel}
         data-sel={sel ? "1" : undefined} data-i={i} title={hit.path}
         onClick={renaming ? undefined : () => h.onSelect(i)} onDoubleClick={renaming ? undefined : () => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      <FileIcon name={hit.name} isDir={hit.is_dir} src={backend.iconUrl(hit.path, hit.is_dir, thumb, top ? 96 : undefined)} thumb={thumb} />
      <div className="ov-meta">
        <div className="ov-name">
          {renaming
            ? <RenameInput initial={hit.name} isDir={hit.is_dir} onCommit={(n) => h.onRenameCommit(i, n)} onCancel={h.onRenameCancel} />
            : <Highlight text={hit.name} query={query} />}
        </div>
        {snippet ? (
          <div className="ov-sub snip" title="Found inside the file"><Snippet text={snippet} /></div>
        ) : top ? (
          <div className="ov-sub">{KIND_LABEL[kind]}{hit.mtime ? ` · ${formatDate(hit.mtime)}` : ""} · {where(hit.path)}</div>
        ) : (
          <div className="ov-sub path"><span className="head">{head}</span><span className="tail">{tail}</span></div>
        )}
      </div>
      {it.sem?.page ? <span className="ov-tag">p. {it.sem.page}</span> : null}
    </div>
  );
});

const Tile = memo(function Tile({ it, i, sel, backend, h }: { it: OvItem; i: number; sel: boolean; backend: Backend; h: ItemHandlers }) {
  const src = backend.iconUrl(it.hit.path, false, true, 256);
  return (
    <div className={"ov-tile" + (sel ? " selected" : "")} role="option" aria-selected={sel} aria-label={it.hit.name}
         data-sel={sel ? "1" : undefined} data-i={i} title={it.hit.name}
         onClick={() => h.onSelect(i)} onDoubleClick={() => h.onOpen(i)}
         onContextMenu={(e) => h.onMenu(i, e)} onMouseDown={(e) => h.onDown(i, e)}>
      {src ? <img src={src} alt="" loading="lazy" draggable={false} /> : <FileIcon name={it.hit.name} isDir={false} src={null} />}
    </div>
  );
});

export function Overview({ sections, sel, renaming, query, backend, expanded, handlers, onExpand, onBack, scrollRef, armed }: {
  sections: OvSection[]; sel: number; renaming: boolean; query: string; backend: Backend; expanded: boolean;
  handlers: ItemHandlers; onExpand: (id: SectionId) => void; onBack: () => void;
  scrollRef: React.RefObject<HTMLDivElement | null>; armed: string | null;
}) {
  let base = 0;
  return (
    <div className="ov" ref={scrollRef} role="listbox" aria-label="Results">
      {sections.map((s) => {
        const start = base;
        base += s.items.length;
        return (
          <section key={s.id} className={"ov-sec s-" + s.id} aria-label={s.title}>
            <div className="ov-head">
              {expanded ? (
                <button className="ov-back" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onBack} aria-label={`Back to all results from ${s.title}`}>
                  <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
                  <span className="ov-title">{s.title}<span className="ov-count">{s.total}</span></span>
                </button>
              ) : (
                <span className="ov-title">{s.title}</span>
              )}
              {s.more && (
                <button className="ov-more" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => onExpand(s.id)}>
                  Show all{s.total > s.items.length ? ` ${s.total.toLocaleString("en-US")}` : ""}
                </button>
              )}
            </div>
            {s.id === "images" ? (
              <div className="ov-grid">
                {s.items.map((it, j) => <Tile key={it.key} it={it} i={start + j} sel={start + j === sel} backend={backend} h={handlers} />)}
              </div>
            ) : (
              s.items.map((it, j) => <Row key={it.key} it={it} i={start + j} sel={start + j === sel} query={query} backend={backend} h={handlers}
                                                renaming={renaming && start + j === sel} armed={armed} />)
            )}
          </section>
        );
      })}
    </div>
  );
}
