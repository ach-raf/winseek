import { memo, useEffect, useRef } from "react";
import type { Backend } from "./types";
import type { OvItem, OvSection, SectionId } from "./overviewLogic";
import { FileIcon } from "./FileIcon";
import { Snippet } from "./Semantic";
import { formatDate, kindOf, matchRanges, splitDir } from "./format";

export function Highlight({ text, query }: { text: string; query: string }) {
  const ranges = matchRanges(text, query);
  if (!ranges.length) return <>{text}</>;
  const out: React.ReactNode[] = [];
  let at = 0;
  ranges.forEach(([s, e], i) => {
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

const Row = memo(function Row({ it, i, sel, query, backend, h, renaming }: {
  it: OvItem; i: number; sel: boolean; query: string; backend: Backend; h: ItemHandlers; renaming: boolean;
}) {
  const { hit } = it;
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

export function Overview({ sections, sel, renaming, query, backend, expanded, handlers, onExpand, onBack, scrollRef }: {
  sections: OvSection[]; sel: number; renaming: boolean; query: string; backend: Backend; expanded: boolean;
  handlers: ItemHandlers; onExpand: (id: SectionId) => void; onBack: () => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
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
              {expanded && <button className="ov-back" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onBack} aria-label="Back to all results">
                <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
              </button>}
              <span className="ov-title">{s.title}{expanded && <span className="ov-count">{s.total}</span>}</span>
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
                                                renaming={renaming && start + j === sel} />)
            )}
          </section>
        );
      })}
    </div>
  );
}
