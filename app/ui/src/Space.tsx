import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import type { Backend, DiskUsage, DriveSpace, Hit, UsageEntry, UsageType } from "./types";
import type { LibActions } from "./Gallery";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize } from "./format";
import { IChevL, IUp, IRefresh } from "./icons";
import { KINDS, crumbs, formatPct, kindTotals, parentPath, pct, squarify, tileItems, withoutHidden, type Rest, type TileItem } from "./spaceLogic";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const num = (n: number) => n.toLocaleString("en-US");
const kc = (k: number): CSSProperties => ({ "--kc": `var(--k${k in KINDS ? k : 0})` } as CSSProperties);
const driveName = (letter: string) => `Local disk (${letter}:)`;

type Tab = "items" | "types" | "files";
const TABS: { id: Tab; label: string }[] = [
  { id: "items", label: "Contents" },
  { id: "types", label: "File types" },
  { id: "files", label: "Biggest files" },
];

/** Where the space went: drives -> folders -> files, as a treemap plus a ranked list. Reads the name index, so it is instant. */
export function Space({ backend, hidden, actions, epoch }: {
  backend: Backend;
  hidden: ReadonlySet<string>;
  actions: LibActions;
  epoch: number;
}) {
  const [path, setPath] = useState("");
  const [data, setData] = useState<DiskUsage | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [sel, setSel] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("items");
  const [again, setAgain] = useState(0);
  const seq = useRef(0);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const n = ++seq.current;
    setLoading(true);
    backend.diskUsage(path, 100).then((d) => {
      if (n !== seq.current) return;
      setData(d); setErr(null); setLoading(false);
    }).catch((e) => {
      if (n !== seq.current) return;
      setErr(errText(e)); setLoading(false);
    });
  }, [backend, path, epoch, again]);

  // Recycled something: the index notices a moment later, so ask again then (the list already hides it).
  const hiddenCount = hidden.size;
  useEffect(() => {
    if (hiddenCount === 0) return;
    const t = window.setTimeout(() => setAgain((a) => a + 1), 1500);
    return () => window.clearTimeout(t);
  }, [hiddenCount]);

  const view = useMemo(() => (data ? withoutHidden(data, hidden) : null), [data, hidden]);
  const go = useCallback((p: string, pick?: string) => { setPath(p); setSel(pick ?? null); setTab("items"); }, []);
  const up = useCallback(() => { if (path) go(parentPath(path), path); }, [path, go]);

  const entries = view?.entries ?? [];
  const selEntry = sel ? entries.find((e) => e.path === sel) ?? null : null;
  const selHit = sel && !selEntry ? view?.biggest.find((b) => b.path === sel) ?? null : null;

  const move = (d: number) => {
    if (entries.length === 0) return;
    const i = sel ? entries.findIndex((e) => e.path === sel) : -1;
    setSel(entries[Math.max(0, Math.min(entries.length - 1, i < 0 ? (d > 0 ? 0 : entries.length - 1) : i + d))].path);
  };
  const recycle = (p: string) => { void actions.recycle([p]); setSel(null); };

  useEffect(() => {
    if (!sel) return;
    root.current?.querySelector<HTMLElement>(`[data-row="${CSS.escape(sel)}"]`)?.scrollIntoView({ block: "nearest" });
  }, [sel, tab]);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey || e.metaKey) return;
    if (e.key === "ArrowDown" || e.key === "ArrowRight") { e.preventDefault(); move(1); }
    else if (e.key === "ArrowUp" || e.key === "ArrowLeft") { e.preventDefault(); move(-1); }
    else if (e.key === "Enter" && selEntry) { e.preventDefault(); if (selEntry.is_dir) go(selEntry.path); else actions.open(selEntry.path); }
    else if (e.key === "Delete" && sel) { e.preventDefault(); recycle(sel); }
    else if ((e.key === "Backspace" || (e.altKey && e.key === "ArrowLeft") || e.key === "Escape") && path) { e.preventDefault(); up(); }
  };

  if (!view) {
    return (
      <div className="su">
        {err
          ? <div className="lg-empty"><strong>Could not read the disk index</strong><span>{err}</span><button className="btn pri" onClick={() => setAgain((a) => a + 1)}>Try again</button></div>
          : <div className="lg-empty"><span className="spin" aria-label="Loading" /></div>}
      </div>
    );
  }

  const atHome = path === "";
  const total = view.size;

  return (
    <div className="su" ref={root} tabIndex={0} data-autofocus onKeyDown={onKey}>
      <header className="su-head">
        <button className="btn sm su-up" disabled={atHome} onClick={up} title="Up one level (Backspace)"><IUp size={15} />Up</button>
        <Crumbs path={path} onGo={go} />
        <span className="su-sp" />
        {loading && <span className="spin" aria-label="Loading" />}
        <button className="btn sm su-ref" onClick={() => setAgain((a) => a + 1)} title="Read the index again"><IRefresh size={14} />Refresh</button>
      </header>
      {err && <div className="su-err" role="alert">{err} <button className="gd-link" onClick={() => setAgain((a) => a + 1)}>Try again</button></div>}
      {atHome ? (
        <Drives data={view} onGo={go} />
      ) : (
        <>
          <Summary data={view} />
          <div className="su-body">
            <section className="su-map" aria-label="Treemap">
              <Treemap data={view} sel={sel} onSel={setSel} onOpen={(e) => (e.is_dir ? go(e.path) : actions.open(e.path))} onMenu={(e, x, y) => actions.menu(e.path, x, y)} />
              <Legend data={view} />
            </section>
            <aside className="su-side">
              <div className="dseg su-tabs" role="tablist" aria-label="What to list">
                {TABS.map((t) => (
                  <button key={t.id} role="tab" aria-selected={tab === t.id} className={tab === t.id ? "on" : ""} onClick={() => setTab(t.id)}>{t.label}</button>
                ))}
              </div>
              <div className="su-list">
                {tab === "items" && <Items backend={backend} data={view} sel={sel} onSel={setSel} onOpen={(e) => (e.is_dir ? go(e.path) : actions.open(e.path))} onMenu={(e, x, y) => actions.menu(e.path, x, y)} />}
                {tab === "types" && <Types types={view.types} total={total} />}
                {tab === "files" && <Biggest backend={backend} data={view} sel={sel} onSel={setSel} onOpen={(h) => actions.open(h.path)} onMenu={(h, x, y) => actions.menu(h.path, x, y)} />}
              </div>
              <Selected
                entry={selEntry} hit={selHit} total={total} actions={actions}
                onEnter={(p) => go(p)} onShow={(h) => go(parentPath(h.path), h.path)} onRecycle={recycle}
              />
            </aside>
          </div>
        </>
      )}
    </div>
  );
}

// ---- header ---------------------------------------------------------------------------------------------------

function Crumbs({ path, onGo }: { path: string; onGo: (p: string) => void }) {
  const all = crumbs(path);
  // Keep the drive and the last three; fold the middle into one "…" that opens the folder just before the last three.
  const fold = all.length > 5 ? all.length - 4 : 0;
  return (
    <nav className="su-crumbs" aria-label="Location">
      {all.map((c, i) => {
        if (fold && i > 1 && i < fold) return i === 2 ? <span key="fold" className="su-cr"><i><IChevL size={12} /></i><button className="su-chip" title={all[fold - 1].path} onClick={() => onGo(all[fold - 1].path)}>…</button></span> : null;
        const last = i === all.length - 1;
        return (
          <span key={c.path || "pc"} className="su-cr">
            {i > 0 && <i><IChevL size={12} /></i>}
            <button className={"su-chip" + (last ? " now" : "")} aria-current={last ? "page" : undefined} title={c.path || "All drives"} onClick={() => !last && onGo(c.path)}>{c.label}</button>
          </span>
        );
      })}
    </nav>
  );
}

function Summary({ data }: { data: DiskUsage }) {
  const drive = data.drives[0];
  const kinds = useMemo(() => kindTotals(data), [data]);
  return (
    <div className="su-sum">
      <div className="su-big">
        <b>{formatSize(data.size) || "0 B"}</b>
        <span>{num(data.files)} file{data.files === 1 ? "" : "s"} · {num(data.folders)} folder{data.folders === 1 ? "" : "s"}</span>
      </div>
      {drive && <Capacity drive={drive} indexed={data.size} kinds={kinds} />}
    </div>
  );
}

/** The drive as a bar: what the index sees, by type, then other used space, then free. */
function Capacity({ drive, indexed, kinds }: { drive: DriveSpace; indexed: number; kinds: number[] }) {
  const used = Math.max(0, drive.total - drive.free);
  const other = Math.max(0, used - indexed);
  return (
    <div className="su-cap">
      <div className="su-capbar" role="img" aria-label={`${formatSize(used)} used of ${formatSize(drive.total)}`}>
        {kinds.map((b, k) => (b > 0 ? <i key={k} style={{ width: `${pct(b, drive.total)}%`, background: `var(--k${k})` }} title={`${KINDS[k].label}: ${formatSize(b)}`} /> : null))}
        {other > 0 && <i className="oth" style={{ width: `${pct(other, drive.total)}%` }} title={`Other used space: ${formatSize(other)}`} />}
      </div>
      <span>{formatSize(used)} used of {formatSize(drive.total)} · <b>{formatSize(drive.free)} free</b>{other > drive.total * 0.02 ? ` · ${formatSize(other)} is system and other space the file list does not cover` : ""}</span>
    </div>
  );
}

// ---- drives (top level) ---------------------------------------------------------------------------------------

function Drives({ data, onGo }: { data: DiskUsage; onGo: (p: string) => void }) {
  return (
    <div className="su-home">
      <h3>Your drives</h3>
      <p>Pick a drive to see what is taking up the room. Everything below comes from the name index, so it opens instantly.</p>
      <div className="su-drives">
        {[...data.entries].sort((a, b) => a.name.localeCompare(b.name)).map((e) => {
          const letter = e.name.slice(0, 1);
          const sp = data.drives.find((d) => d.letter === letter);
          const used = sp ? Math.max(0, sp.total - sp.free) : e.size;
          const full = sp && sp.total > 0 ? used / sp.total : 0;
          return (
            <button key={e.path} className="su-drive" onClick={() => onGo(e.path)}>
              <span className="su-dl">{letter}</span>
              <span className="su-dn">
                <b>{driveName(letter)}</b>
                <small>{num(e.files)} files · {num(e.folders)} folders</small>
              </span>
              {sp ? (
                <>
                  <span className={"su-dbar" + (full > 0.9 ? " hot" : "")}><i style={{ width: `${Math.min(100, full * 100)}%` }} /></span>
                  <span className="su-dfoot"><b>{formatSize(sp.free)} free</b> of {formatSize(sp.total)}</span>
                </>
              ) : <span className="su-dfoot"><b>{formatSize(e.size)}</b> in files</span>}
            </button>
          );
        })}
        {data.entries.length === 0 && <div className="su-none">No drives are indexed yet.</div>}
      </div>
    </div>
  );
}

// ---- treemap ----------------------------------------------------------------------------------------------------

const GAP = 3;

function Treemap({ data, sel, onSel, onOpen, onMenu }: {
  data: DiskUsage; sel: string | null;
  onSel: (p: string) => void; onOpen: (e: UsageEntry) => void; onMenu: (e: UsageEntry, x: number, y: number) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [dim, setDim] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setDim({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setDim({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const tiles = useMemo(() => {
    const r = { x: 0, y: 0, w: dim.w, h: dim.h };
    return squarify(tileItems(data, r), r);
  }, [data, dim.w, dim.h]);
  const total = data.entries.reduce((a, e) => a + e.size, 0) + data.more_size;

  return (
    <div className="su-tm" ref={box}>
      {tiles.map((t) => {
        const it: TileItem = t.item;
        const w = Math.max(0, t.w - GAP), h = Math.max(0, t.h - GAP);
        const st: CSSProperties = { left: t.x + GAP / 2, top: t.y + GAP / 2, width: w, height: h };
        if ("rest" in it) {
          const r = it as Rest;
          return (
            <div key="rest" className="su-tile rest" style={st} title={`${num(r.count)} smaller items · ${formatSize(r.size)} together`}>
              {w > 90 && h > 34 && <span className="su-tl"><b>{num(r.count)} smaller items</b><em>{formatSize(r.size)}</em></span>}
            </div>
          );
        }
        const e = it as UsageEntry;
        return (
          <button
            key={e.path} data-tile={e.path} style={{ ...st, ...kc(e.kind) }}
            className={"su-tile" + (e.is_dir ? " dir" : "") + (sel === e.path ? " on" : "")}
            title={`${e.name}\n${formatSize(e.size)} · ${formatPct(pct(e.size, total))}${e.is_dir ? ` · ${num(e.files)} files` : ""}`}
            tabIndex={-1}
            onClick={() => onSel(e.path)} onDoubleClick={() => onOpen(e)}
            onContextMenu={(ev) => { ev.preventDefault(); onSel(e.path); onMenu(e, ev.clientX, ev.clientY); }}
          >
            {w > 64 && h > 30 && (
              <span className="su-tl">
                <b>{e.name}</b>
                {h > 48 && <em>{formatSize(e.size)}{w > 150 ? ` · ${formatPct(pct(e.size, total))}` : ""}</em>}
              </span>
            )}
          </button>
        );
      })}
      {tiles.length === 0 && dim.w > 0 && <div className="su-none">{data.entries.length === 0 ? "This folder is empty." : "Nothing in here takes up space."}</div>}
    </div>
  );
}

function Legend({ data }: { data: DiskUsage }) {
  const kinds = useMemo(() => kindTotals(data), [data]);
  const shown = KINDS.filter((k) => kinds[k.id] > 0).sort((a, b) => kinds[b.id] - kinds[a.id]);
  return (
    <div className="su-legend">
      {shown.map((k) => <span key={k.id}><i style={{ background: `var(--k${k.id})` }} />{k.label}<em>{formatSize(kinds[k.id])}</em></span>)}
      <span className="su-hint">Bright blocks are folders · double-click to open</span>
    </div>
  );
}

// ---- lists --------------------------------------------------------------------------------------------------------

function Items({ backend, data, sel, onSel, onOpen, onMenu }: {
  backend: Backend; data: DiskUsage; sel: string | null;
  onSel: (p: string) => void; onOpen: (e: UsageEntry) => void; onMenu: (e: UsageEntry, x: number, y: number) => void;
}) {
  const total = data.size;
  if (data.entries.length === 0) return <div className="su-none">This folder is empty.</div>;
  return (
    <div role="listbox" aria-label="Contents">
      {data.entries.map((e) => {
        const p = pct(e.size, total);
        return (
          <div
            key={e.path} role="option" aria-selected={sel === e.path} data-row={e.path} style={kc(e.kind)}
            className={"su-row" + (sel === e.path ? " on" : "")}
            onClick={() => onSel(e.path)} onDoubleClick={() => onOpen(e)}
            onContextMenu={(ev) => { ev.preventDefault(); onSel(e.path); onMenu(e, ev.clientX, ev.clientY); }}
          >
            <i className="su-fill" style={{ width: `${Math.min(100, p)}%` }} />
            <FileIcon name={e.name} isDir={e.is_dir} src={backend.iconUrl(e.path, e.is_dir, false)} />
            <span className="su-rn"><b title={e.name}>{e.name}</b><small>{e.is_dir ? `${num(e.files)} file${e.files === 1 ? "" : "s"}` : e.mtime ? formatDate(e.mtime) : ""}</small></span>
            <span className="su-rs"><b>{formatSize(e.size) || "0 B"}</b><small>{formatPct(p)}</small></span>
          </div>
        );
      })}
      {data.more > 0 && <div className="su-more">+ {num(data.more)} smaller items · {formatSize(data.more_size)}</div>}
    </div>
  );
}

function Types({ types, total }: { types: UsageType[]; total: number }) {
  if (types.length === 0) return <div className="su-none">No files here.</div>;
  return (
    <div>
      {types.map((t) => {
        const p = pct(t.bytes, total);
        return (
          <div key={t.ext || "(none)"} className="su-row ty" style={kc(t.kind)}>
            <i className="su-fill" style={{ width: `${Math.min(100, p)}%` }} />
            <span className="su-dot" />
            <span className="su-rn"><b>{t.ext ? `.${t.ext}` : "No extension"}</b><small>{KINDS[t.kind]?.label ?? "Other"} · {num(t.files)} file{t.files === 1 ? "" : "s"}</small></span>
            <span className="su-rs"><b>{formatSize(t.bytes) || "0 B"}</b><small>{formatPct(p)}</small></span>
          </div>
        );
      })}
    </div>
  );
}

function Biggest({ backend, data, sel, onSel, onOpen, onMenu }: {
  backend: Backend; data: DiskUsage; sel: string | null;
  onSel: (p: string) => void; onOpen: (h: Hit) => void; onMenu: (h: Hit, x: number, y: number) => void;
}) {
  if (data.biggest.length === 0) return <div className="su-none">No files here.</div>;
  const base = data.path.replace(/\\$/, "").length + 1;
  return (
    <div role="listbox" aria-label="Biggest files">
      {data.biggest.map((h) => {
        const rel = h.path.slice(base);
        const cut = rel.lastIndexOf("\\");
        return (
          <div
            key={h.path} role="option" aria-selected={sel === h.path} data-row={h.path}
            className={"su-row" + (sel === h.path ? " on" : "")}
            onClick={() => onSel(h.path)} onDoubleClick={() => onOpen(h)}
            onContextMenu={(ev) => { ev.preventDefault(); onSel(h.path); onMenu(h, ev.clientX, ev.clientY); }}
          >
            <FileIcon name={h.name} isDir={false} src={backend.iconUrl(h.path, false, false)} />
            <span className="su-rn"><b title={h.name}>{h.name}</b><small title={h.path}>{cut > 0 ? rel.slice(0, cut) : "In this folder"}</small></span>
            <span className="su-rs"><b>{formatSize(h.size) || "0 B"}</b><small>{h.mtime ? formatDate(h.mtime) : ""}</small></span>
          </div>
        );
      })}
    </div>
  );
}

function Selected({ entry, hit, total, actions, onEnter, onShow, onRecycle }: {
  entry: UsageEntry | null; hit: Hit | null; total: number; actions: LibActions;
  onEnter: (p: string) => void; onShow: (h: Hit) => void; onRecycle: (p: string) => void;
}) {
  const path = entry?.path ?? hit?.path;
  if (!path) return <div className="su-sel idle">Select a block or a row to see what you can do with it.</div>;
  const name = entry?.name ?? hit!.name;
  const size = entry?.size ?? hit!.size ?? 0;
  const mtime = entry?.mtime ?? hit?.mtime ?? null;
  const meta = [
    formatSize(size) || "0 B",
    `${formatPct(pct(size, total))} of this folder`,
    entry?.is_dir ? `${num(entry.files)} files` : null,
    mtime ? `Modified ${formatDate(mtime)}` : null,
  ].filter(Boolean).join(" · ");
  return (
    <div className="su-sel">
      <div className="su-st"><b title={name}>{name}</b><small title={path}>{path}</small><small>{meta}</small></div>
      <div className="su-sb">
        {entry?.is_dir && <button className="btn sm" onClick={() => onEnter(entry.path)}>Open here</button>}
        {hit && <button className="btn sm" onClick={() => onShow(hit)}>Show in folder</button>}
        {!entry?.is_dir && <button className="btn sm" onClick={() => actions.open(path)}>Open</button>}
        <button className="btn sm" onClick={() => actions.reveal(path)}>Reveal</button>
        <button className="btn sm su-del" onClick={() => onRecycle(path)} title="Move to Recycle Bin (Delete)">Recycle</button>
      </div>
    </div>
  );
}
