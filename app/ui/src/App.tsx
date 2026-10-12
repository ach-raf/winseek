import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Filter, Hit, Sort, VolumeStatus } from "./types";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize, matchRanges, splitDir } from "./format";

const ROW_H = 54;
const PAGE = 80;
const OVERSCAN = 6;

const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "images", label: "Images" },
  { id: "docs", label: "Docs" },
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

function Highlight({ text, query }: { text: string; query: string }) {
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

const Row = memo(function Row({
  hit, query, selected, top, onPick, onHover,
}: {
  hit: Hit | undefined; query: string; selected: boolean; top: number; onPick: () => void; onHover: () => void;
}) {
  if (!hit) {
    return (
      <div className="row skeleton" style={{ transform: `translateY(${top}px)` }}>
        <span className="ficon" />
        <div className="meta"><span className="sk w1" /><span className="sk w2" /></div>
      </div>
    );
  }
  const { head, tail } = splitDir(hit.path);
  return (
    <div
      className={"row" + (selected ? " selected" : "")}
      style={{ transform: `translateY(${top}px)` }}
      role="option"
      aria-selected={selected}
      onClick={onPick}
      onMouseMove={onHover}
      title={hit.path}
    >
      <FileIcon name={hit.name} isDir={hit.is_dir} />
      <div className="meta">
        <div className="name"><Highlight text={hit.name} query={query} /></div>
        <div className="path"><span className="head">{head}</span><span className="tail">{tail}</span></div>
      </div>
      <div className="side">
        <span className="size">{hit.is_dir ? "Folder" : formatSize(hit.size)}</span>
        <span className="date">{formatDate(hit.mtime)}</span>
      </div>
    </div>
  );
});

interface Data { total: number; rows: (Hit | undefined)[]; elapsedMs: number | null; recent: boolean }

export default function App({ backend }: { backend: Backend }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("rel");
  const [sel, setSel] = useState(0);
  const [data, setData] = useState<Data>({ total: 0, rows: [], elapsedMs: null, recent: true });
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<VolumeStatus[]>([]);
  const [toast, setToast] = useState<string | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(360);
  const [epoch, setEpoch] = useState(0); // bumped when the popup is re-shown

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const seq = useRef(0);
  const inflight = useRef(new Set<number>());
  const rowsRef = useRef<(Hit | undefined)[]>([]);
  const toastTimer = useRef<number | undefined>(undefined);

  const isRecent = query.trim() === "" && filter === "all" && sort === "rel";

  // ---- data loading --------------------------------------------------------
  useEffect(() => {
    const my = ++seq.current;
    inflight.current.clear();
    (async () => {
      if (isRecent) {
        const hits = await backend.recent(40);
        if (my !== seq.current) return;
        rowsRef.current = hits;
        setData({ total: hits.length, rows: hits, elapsedMs: null, recent: true });
      } else {
        const r = await backend.search({ query, offset: 0, limit: PAGE, sort, filter });
        if (my !== seq.current) return;
        const rows: (Hit | undefined)[] = new Array(r.total).fill(undefined);
        r.hits.forEach((h, i) => (rows[i] = h));
        rowsRef.current = rows;
        setData({ total: r.total, rows, elapsedMs: r.elapsed_us / 1000, recent: false });
      }
      setLoaded(true);
      setSel(0);
      setScrollTop(0);
      if (listRef.current) listRef.current.scrollTop = 0;
    })().catch(console.error);
  }, [backend, query, filter, sort, isRecent, epoch]);

  const ensurePages = useCallback((first: number, last: number) => {
    if (data.recent) return;
    const my = seq.current;
    for (let p = Math.floor(first / PAGE); p <= Math.floor(last / PAGE); p++) {
      if (rowsRef.current[p * PAGE] || inflight.current.has(p) || p * PAGE >= data.total) continue;
      inflight.current.add(p);
      backend.search({ query, offset: p * PAGE, limit: PAGE, sort, filter }).then((r) => {
        if (my !== seq.current) return;
        const rows = rowsRef.current.slice();
        r.hits.forEach((h, i) => (rows[p * PAGE + i] = h));
        rowsRef.current = rows;
        setData((d) => ({ ...d, rows }));
      }).catch(console.error);
    }
  }, [backend, data.recent, data.total, query, sort, filter]);

  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(data.total - 1, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);
  useEffect(() => { if (last >= first) ensurePages(first, last); }, [first, last, ensurePages]);

  // ---- status / window events ---------------------------------------------
  useEffect(() => {
    const refresh = () => backend.status().then(setStatus).catch(() => {});
    refresh();
    const t = window.setInterval(refresh, 5000);
    return () => window.clearInterval(t);
  }, [backend, epoch]);

  useEffect(() => {
    let off: (() => void) | undefined;
    backend.onShown(() => {
      setEpoch((e) => e + 1);
      requestAnimationFrame(() => { inputRef.current?.focus(); inputRef.current?.select(); });
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend]);

  useEffect(() => { inputRef.current?.focus(); }, []);

  useLayoutEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // ---- actions -------------------------------------------------------------
  const flash = useCallback((msg: string) => {
    setToast(msg);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 1600);
  }, []);

  const current = data.rows[sel];

  const act = useCallback(async (kind: "open" | "reveal" | "copy", h: Hit | undefined) => {
    if (!h) return;
    try {
      if (kind === "copy") { await backend.copyPath(h.path); flash("Path copied"); return; }
      if (kind === "open") await backend.openPath(h.path); else await backend.revealPath(h.path);
      await backend.hide();
    } catch (e) { flash(String(e)); }
  }, [backend, flash]);

  const move = useCallback((to: number) => {
    const n = Math.max(0, Math.min(data.total - 1, to));
    setSel(n);
    const el = listRef.current;
    if (!el) return;
    const top = n * ROW_H, bottom = top + ROW_H;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }, [data.total]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const page = Math.max(1, Math.floor(viewH / ROW_H) - 1);
      switch (e.key) {
        case "ArrowDown": e.preventDefault(); move(sel + 1); break;
        case "ArrowUp": e.preventDefault(); move(sel - 1); break;
        case "PageDown": e.preventDefault(); move(sel + page); break;
        case "PageUp": e.preventDefault(); move(sel - page); break;
        case "Home": if (e.ctrlKey) { e.preventDefault(); move(0); } break;
        case "End": if (e.ctrlKey) { e.preventDefault(); move(data.total - 1); } break;
        case "Enter": e.preventDefault(); void act(e.ctrlKey ? "reveal" : "open", current); break;
        case "Escape": e.preventDefault(); void backend.hide(); break;
        case "Tab": {
          e.preventDefault();
          const i = FILTERS.findIndex((f) => f.id === filter);
          setFilter(FILTERS[(i + (e.shiftKey ? FILTERS.length - 1 : 1)) % FILTERS.length].id);
          break;
        }
        case "c": case "C":
          if (e.ctrlKey && e.shiftKey) { e.preventDefault(); void act("copy", current); }
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [move, sel, viewH, act, current, filter, data.total, backend]);

  // ---- render --------------------------------------------------------------
  const totalFiles = status.reduce((a, v) => a + v.files + v.folders, 0);
  const state = status.find((v) => v.state !== "live")?.state ?? (status.length ? "live" : "connecting");
  const statusText =
    state === "demo" ? "demo data — searchd not running"
    : state === "offline" ? "searchd offline — reconnecting…"
    : totalFiles ? `${totalFiles.toLocaleString("en-US")} files · ${state}`
    : state === "scanning" ? "Indexing…"
    : "Connecting…";
  const heading = data.recent
    ? "Recent"
    : query.trim() === ""
      ? `Recent · ${FILTERS.find((f) => f.id === filter)?.label}`
      : null;
  const sortLabel = SORTS.find((s) => s.id === sort)!.label;
  const visible = useMemo(() => {
    const out: React.ReactNode[] = [];
    for (let i = first; i <= last; i++) {
      out.push(
        <Row key={i} hit={data.rows[i]} query={query} selected={i === sel} top={i * ROW_H}
             onPick={() => { setSel(i); void act("open", data.rows[i]); }} onHover={() => setSel((s) => (s === i ? s : i))} />,
      );
    }
    return out;
  }, [first, last, data.rows, query, sel, act]);

  return (
    <div className="shell">
      <header className="searchbar" data-tauri-drag-region>
        <svg className="glass" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
          <circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" />
        </svg>
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search files and folders"
          spellCheck={false}
          autoComplete="off"
          autoFocus
          aria-label="Search"
        />
        {query && (
          <button className="clear" onClick={() => { setQuery(""); inputRef.current?.focus(); }} aria-label="Clear" tabIndex={-1}>
            <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
          </button>
        )}
        <kbd className="hotkey">Alt Space</kbd>
      </header>

      <div className="chips" role="tablist">
        {FILTERS.map((f) => (
          <button key={f.id} role="tab" aria-selected={filter === f.id} className={"chip" + (filter === f.id ? " on" : "")}
                  tabIndex={-1} onClick={() => { setFilter(f.id); inputRef.current?.focus(); }}>
            {f.label}
          </button>
        ))}
        <span className="spacer" />
        <button className="sort" tabIndex={-1} title="Change sort order"
                onClick={() => { setSort(SORTS[(SORTS.findIndex((s) => s.id === sort) + 1) % SORTS.length].id); inputRef.current?.focus(); }}>
          {sortLabel}
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m7 10 5 5 5-5" /></svg>
        </button>
      </div>

      {heading && <div className="section">{heading}</div>}

      <div className="list" ref={listRef} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)} role="listbox">
        <div className="spacer-v" style={{ height: data.total * ROW_H }}>{visible}</div>
        {loaded && data.total === 0 && (
          <div className="empty">
            <svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21M8 10.5h5" /></svg>
            <strong>No results{query.trim() ? <> for &ldquo;{query.trim()}&rdquo;</> : null}</strong>
            <span>Check the spelling, or try a different filter.</span>
          </div>
        )}
      </div>

      <footer className="footer">
        <span className={"status s-" + state}><i className="dot" />{statusText}</span>
        {toast ? (
          <span className="toast" role="status">{toast}</span>
        ) : (
          !data.recent && loaded && <span className="count">{data.total.toLocaleString("en-US")} result{data.total === 1 ? "" : "s"}{data.elapsedMs !== null && ` · ${data.elapsedMs < 1 ? "<1" : Math.round(data.elapsedMs)} ms`}</span>
        )}
        <span className="spacer" />
        <span className="hints">
          <span><kbd>↵</kbd> Open</span>
          <span><kbd>Ctrl ↵</kbd> Reveal</span>
          <span><kbd>Ctrl ⇧ C</kbd> Copy path</span>
        </span>
      </footer>
    </div>
  );
}
