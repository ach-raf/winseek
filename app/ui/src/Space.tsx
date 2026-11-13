import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import type { Backend, DiskUsage, DriveSpace, Hit, UsageDetail, UsageEntry, UsageType } from "./types";
import type { LibActions } from "./Gallery";
import { FileIcon } from "./FileIcon";
import { formatDate, formatSize } from "./format";
import { IChevL, IUp, IRefresh } from "./icons";
import { uniformRange } from "./libraryLogic";
import {
  KINDS, chainTo, crumbs, detailWithoutHidden, driveTitle, flattenTree, formatPct, kindTotals, parentPath, pct, squarify, tileItems,
  within, withoutHidden, type Rest, type TileItem, type TreeRow,
} from "./spaceLogic";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const num = (n: number) => n.toLocaleString("en-US");
const kc = (k: number): CSSProperties => ({ "--kc": `var(--k${k in KINDS ? k : 0})` } as CSSProperties);
const ROW_H = 32;

type Tab = "tree" | "types" | "files";
const TABS: { id: Tab; label: string }[] = [
  { id: "tree", label: "Folders" },
  { id: "types", label: "File types" },
  { id: "files", label: "Biggest files" },
];

/**
 * Where the space went, the WizTree way: a folder tree with sizes on the left, a treemap of the chosen folder on the right.
 * Everything comes from the name index through one cached roll-up, so opening a folder takes a few milliseconds.
 */
export function Space({ backend, hidden, actions, epoch }: {
  backend: Backend;
  hidden: ReadonlySet<string>;
  actions: LibActions;
  epoch: number;
}) {
  const [root, setRoot] = useState("");          // where the tree starts ("" = the drive list)
  const [focus, setFocus] = useState("");        // the folder the treemap shows
  const [sel, setSel] = useState<string | null>(null);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [tab, setTab] = useState<Tab>("tree");
  const [ver, bump] = useState(0);
  const [err, setErr] = useState<string | null>(null);
  const cache = useRef(new Map<string, DiskUsage>());
  const flying = useRef(new Set<string>());
  const details = useRef(new Map<string, UsageDetail | null>());
  const rootEl = useRef<HTMLDivElement>(null);

  const fetchDir = useCallback((p: string, force = false) => {
    if (!force && (cache.current.has(p) || flying.current.has(p))) return;
    flying.current.add(p);
    backend.diskUsage(p).then((d) => {
      cache.current.set(p, d); setErr(null);
    }).catch((e) => setErr(errText(e))).finally(() => { flying.current.delete(p); bump((n) => n + 1); });
  }, [backend]);
  const fetchDetail = useCallback((p: string, force = false) => {
    const k = "d:" + p;
    if (!force && (details.current.has(p) || flying.current.has(k))) return;
    flying.current.add(k);
    backend.diskUsageDetail(p, 100).then((d) => { details.current.set(p, d); })
      .catch((e) => { details.current.set(p, null); setErr(errText(e)); })
      .finally(() => { flying.current.delete(k); bump((n) => n + 1); });
  }, [backend]);

  // Read whatever the tree and the treemap need (each folder once).
  useEffect(() => {
    fetchDir("");
    if (root) { fetchDir(root); fetchDir(focus); open.forEach((p) => fetchDir(p)); }
  }, [root, focus, open, fetchDir]);
  useEffect(() => { if (root && tab !== "tree") fetchDetail(focus); }, [root, focus, tab, fetchDetail]);

  // The index changed (recycled something, undo, new scan): read again what is on screen, keep showing the old meanwhile.
  const again = useCallback(() => {
    for (const p of [...cache.current.keys()]) fetchDir(p, true);
    for (const p of [...details.current.keys()]) fetchDetail(p, true);
  }, [fetchDir, fetchDetail]);
  const firstEpoch = useRef(epoch);
  useEffect(() => { if (epoch !== firstEpoch.current) again(); }, [epoch, again]);
  const hiddenCount = hidden.size;
  useEffect(() => {
    if (hiddenCount === 0) return;
    const t = window.setTimeout(again, 1500);
    return () => window.clearTimeout(t);
  }, [hiddenCount, again]);

  // `ver` changes whenever a folder arrives, which is what makes these re-read the cache.
  const get = useCallback((p: string): DiskUsage | undefined => {
    const d = cache.current.get(p);
    return d ? withoutHidden(d, hidden) : undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hidden, ver]);
  const rows = useMemo(() => (root ? flattenTree(root, get, open) : []), [root, open, get]);
  const homeData = get("");
  const mapData = root ? get(focus) : undefined;
  const lastMap = useRef<DiskUsage | undefined>(undefined);
  if (mapData) lastMap.current = mapData;
  const shownMap = mapData ?? (root ? lastMap.current : undefined);

  // ---- navigation ---------------------------------------------------------------------------------------------
  const openChain = useCallback((path: string, rootPath: string) => {
    setOpen((s) => { const n = new Set(s); for (const c of chainTo(rootPath, path)) n.add(c); return n; });
  }, []);
  const enterRoot = useCallback((p: string, pick?: string) => {
    setRoot(p); setFocus(p); setSel(pick ?? null); setTab("tree");
    setOpen(new Set(p ? [p] : []));
    if (p && pick) openChain(pick, p);
  }, [openChain]);
  const showFolder = useCallback((p: string) => {
    if (!root || !within(root, p)) { enterRoot(p); return; }
    setFocus(p); setSel(p); setTab("tree"); openChain(p, root);
    setOpen((s) => new Set(s).add(p));
  }, [root, enterRoot, openChain]);
  const pick = useCallback((e: { path: string; is_dir: boolean }) => {
    setSel(e.path);
    if (e.is_dir) setFocus(e.path);
  }, []);
  const toggle = useCallback((p: string) => setOpen((s) => { const n = new Set(s); if (n.has(p)) n.delete(p); else n.add(p); return n; }), []);
  const up = useCallback(() => {
    if (!root) return;
    if (focus !== root) { const p = parentPath(focus); setFocus(p); setSel(focus); return; }
    const p = parentPath(root);
    if (!p) { enterRoot("", root); return; }
    enterRoot(p, root);
  }, [root, focus, enterRoot]);
  const showFile = useCallback((h: Hit) => {
    const dir = parentPath(h.path);
    setTab("tree"); setFocus(dir); setSel(h.path); if (root) { openChain(h.path, root); }
  }, [root, openChain]);

  const recycle = (p: string) => { void actions.recycle([p]); setSel(null); };

  // ---- keyboard (tree) ---------------------------------------------------------------------------------------
  const nodeRows = useMemo(() => rows.filter((r) => r.type === "node"), [rows]);
  const selIdx = sel ? nodeRows.findIndex((r) => r.entry!.path.toLowerCase() === sel.toLowerCase()) : -1;
  const goRow = (i: number) => {
    const r = nodeRows[Math.max(0, Math.min(nodeRows.length - 1, i))];
    if (r) pick(r.entry!);
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey || e.metaKey) return;
    const cur = selIdx >= 0 ? nodeRows[selIdx] : undefined;
    if (e.key === "ArrowDown" && tab === "tree") { e.preventDefault(); goRow(selIdx < 0 ? 0 : selIdx + 1); }
    else if (e.key === "ArrowUp" && tab === "tree") { e.preventDefault(); goRow(selIdx < 0 ? 0 : selIdx - 1); }
    else if (e.key === "Home" && tab === "tree") { e.preventDefault(); goRow(0); }
    else if (e.key === "End" && tab === "tree") { e.preventDefault(); goRow(nodeRows.length - 1); }
    else if (e.key === "ArrowRight" && cur?.entry!.is_dir) {
      e.preventDefault();
      if (!cur.open) toggle(cur.entry!.path); else goRow(selIdx + 1);
    } else if (e.key === "ArrowLeft" && cur) {
      e.preventDefault();
      if (cur.entry!.is_dir && cur.open && cur.depth > 0) toggle(cur.entry!.path);
      else { const pr = parentPath(cur.entry!.path); const i = nodeRows.findIndex((r) => r.entry!.path === pr); if (i >= 0) goRow(i); }
    } else if (e.key === " " && cur?.entry!.is_dir) { e.preventDefault(); toggle(cur.entry!.path); }
    else if (e.key === "Enter" && cur) { e.preventDefault(); if (cur.entry!.is_dir) enterRoot(cur.entry!.path); else actions.open(cur.entry!.path); }
    else if (e.key === "Delete" && sel) { e.preventDefault(); recycle(sel); }
    else if ((e.key === "Backspace" || (e.altKey && e.key === "ArrowLeft") || e.key === "Escape") && root) { e.preventDefault(); up(); }
  };

  const atHome = root === "";
  if (atHome) {
    return (
      <div className="su" ref={rootEl} tabIndex={0} data-autofocus onKeyDown={onKey}>
        {err && !homeData && <div className="lg-empty"><strong>Could not read the disk index</strong><span>{err}</span><button className="btn pri" onClick={() => fetchDir("", true)}>Try again</button></div>}
        {!homeData && !err && <div className="lg-empty"><span className="spin" aria-label="Loading" /></div>}
        {homeData && <Drives data={homeData} onGo={(p) => enterRoot(p)} />}
      </div>
    );
  }

  const selRow = selIdx >= 0 ? nodeRows[selIdx].entry! : null;
  const detail = tab !== "tree" ? details.current.get(focus) : undefined;
  const detailView = detail ? detailWithoutHidden(detail, hidden) : detail;
  const selHit = sel && (tab === "files" || !selRow) ? detailView?.biggest.find((b) => b.path === sel) ?? null : null;
  const mapSel = shownMap && sel && shownMap.entries.some((e) => e.path.toLowerCase() === sel.toLowerCase()) ? sel : null;

  return (
    <div className="su" ref={rootEl} tabIndex={0} data-autofocus onKeyDown={onKey}>
      <header className="su-head">
        <button className="btn sm su-up" onClick={up} title="Up one level (Backspace)"><IUp size={15} />Up</button>
        <Crumbs path={focus} onGo={(p) => (p === "" ? enterRoot("") : showFolder(p))} />
        <span className="su-sp" />
        <button className="btn sm su-ref" onClick={again} title="Read the index again"><IRefresh size={14} />Refresh</button>
      </header>
      {err && <div className="su-err" role="alert">{err} <button className="gd-link" onClick={again}>Try again</button></div>}
      {shownMap && <Summary data={shownMap} />}
      <div className="su-body">
        <aside className="su-side">
          <div className="dseg su-tabs" role="tablist" aria-label="What to list">
            {TABS.map((t) => <button key={t.id} role="tab" aria-selected={tab === t.id} className={tab === t.id ? "on" : ""} onClick={() => setTab(t.id)}>{t.label}</button>)}
          </div>
          {tab === "tree" && (
            <Tree
              backend={backend} rows={rows} sel={sel} focus={focus}
              onPick={pick} onToggle={toggle} onEnter={(e) => (e.is_dir ? enterRoot(e.path) : actions.open(e.path))}
              onMenu={(e, x, y) => actions.menu(e.path, x, y)}
            />
          )}
          {tab === "types" && <div className="su-list"><Types detail={detailView} total={shownMap?.size ?? 0} /></div>}
          {tab === "files" && <div className="su-list"><Biggest backend={backend} detail={detailView} base={focus} sel={sel} onSel={setSel} onOpen={(h) => actions.open(h.path)} onMenu={(h, x, y) => actions.menu(h.path, x, y)} /></div>}
          <Selected
            entry={selRow} hit={selHit} total={shownMap?.size ?? 0} actions={actions}
            onEnter={(p) => enterRoot(p)} onShow={showFile} onRecycle={recycle}
          />
        </aside>
        <section className="su-map" aria-label="Treemap">
          {shownMap
            ? <Treemap data={shownMap} sel={mapSel} dim={!mapData} onSel={(e) => { setSel(e.path); if (root) openChain(e.path, root); }} onOpen={(e) => (e.is_dir ? showFolder(e.path) : actions.open(e.path))} onMenu={(e, x, y) => actions.menu(e.path, x, y)} />
            : <div className="su-tm"><div className="su-none"><span className="spin" aria-label="Loading" /></div></div>}
          {shownMap && <Legend data={shownMap} />}
        </section>
      </div>
    </div>
  );
}

// ---- header ---------------------------------------------------------------------------------------------------

function Crumbs({ path, onGo }: { path: string; onGo: (p: string) => void }) {
  const all = crumbs(path);
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
      <span>{formatSize(used)} used of {formatSize(drive.total)} · <b>{formatSize(drive.free)} free</b>{other > drive.total * 0.02 ? ` · ${formatSize(other)} is system space and files the list cannot see` : ""}</span>
    </div>
  );
}

// ---- drives (top level) ---------------------------------------------------------------------------------------

function Drives({ data, onGo }: { data: DiskUsage; onGo: (p: string) => void }) {
  return (
    <div className="su-home">
      <h3>Your drives</h3>
      <p>Pick a drive to see what is taking up the room. It reads the name index, so it opens instantly.</p>
      <div className="su-drives">
        {data.entries.map((e) => {
          const letter = e.name.slice(0, 1);
          const sp: DriveSpace | undefined = data.drives.find((d) => d.letter === letter);
          const used = sp ? Math.max(0, sp.total - sp.free) : e.size;
          const full = sp && sp.total > 0 ? used / sp.total : 0;
          return (
            <button key={e.path} className="su-drive" onClick={() => onGo(e.path)}>
              <span className="su-dl">{letter}</span>
              <span className="su-dn">
                <b>{driveTitle(letter, sp?.label)}</b>
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

// ---- the folder tree --------------------------------------------------------------------------------------------

function Tree({ backend, rows, sel, focus, onPick, onToggle, onEnter, onMenu }: {
  backend: Backend; rows: TreeRow[]; sel: string | null; focus: string;
  onPick: (e: UsageEntry) => void; onToggle: (p: string) => void; onEnter: (e: UsageEntry) => void; onMenu: (e: UsageEntry, x: number, y: number) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(0);
  const [h, setH] = useState(400);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el); setH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const selLow = sel?.toLowerCase();
  const idx = selLow ? rows.findIndex((r) => r.type === "node" && r.entry!.path.toLowerCase() === selLow) : -1;
  // keep the selection on screen when it moves (keyboard, a treemap click, "show in folder")
  useEffect(() => {
    const el = box.current;
    if (!el || idx < 0) return;
    if (idx * ROW_H < el.scrollTop) el.scrollTop = idx * ROW_H;
    else if ((idx + 1) * ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = (idx + 1) * ROW_H - el.clientHeight;
  }, [idx, sel]);

  const [from, to] = uniformRange(rows.length, ROW_H, top, h);
  return (
    <div className="su-tree" ref={box} onScroll={(e) => setTop(e.currentTarget.scrollTop)} role="tree" aria-label="Folders">
      <div style={{ height: rows.length * ROW_H, position: "relative" }}>
        {rows.slice(from, to).map((r, k) => {
          const y = (from + k) * ROW_H;
          const pad = { paddingLeft: 8 + r.depth * 16 };
          if (r.type === "busy") return <div key={r.key} className="su-tr dim" style={{ top: y, height: ROW_H, ...pad }}><span className="spin" aria-label="Loading" /></div>;
          if (r.type === "more") return <div key={r.key} className="su-tr dim" style={{ top: y, height: ROW_H, ...pad }}><span className="su-car" />+ {num(r.more!.count)} smaller items · {formatSize(r.more!.size)}</div>;
          const e = r.entry!;
          const p = pct(e.size, r.base);
          const on = e.path.toLowerCase() === selLow;
          return (
            <div
              key={r.key} role="treeitem" aria-expanded={e.is_dir ? !!r.open : undefined} aria-selected={on}
              className={"su-tr" + (on ? " on" : "") + (e.path === focus ? " foc" : "")}
              style={{ top: y, height: ROW_H, ...pad, ...kc(e.kind) }}
              onClick={() => onPick(e)} onDoubleClick={() => onEnter(e)}
              onContextMenu={(ev) => { ev.preventDefault(); onPick(e); onMenu(e, ev.clientX, ev.clientY); }}
              title={`${e.path}\n${formatSize(e.size)} · ${num(e.files)} file${e.files === 1 ? "" : "s"}`}
            >
              {e.is_dir && (e.files > 0 || e.folders > 0)
                ? <button className={"su-car" + (r.open ? " open" : "")} tabIndex={-1} aria-label={r.open ? "Collapse" : "Expand"} onClick={(ev) => { ev.stopPropagation(); onToggle(e.path); }}><IChevL size={12} /></button>
                : <span className="su-car" />}
              <FileIcon name={e.name} isDir={e.is_dir} src={e.is_dir ? null : backend.iconUrl(e.path, false, false)} />
              <span className="su-tn">{e.name}</span>
              <span className="su-tp"><i style={{ width: `${Math.min(100, p)}%` }} /><em>{formatPct(p)}</em></span>
              <span className="su-ts">{formatSize(e.size) || "0 B"}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ---- treemap ----------------------------------------------------------------------------------------------------

const GAP = 3;

function Treemap({ data, sel, dim, onSel, onOpen, onMenu }: {
  data: DiskUsage; sel: string | null; dim: boolean;
  onSel: (e: UsageEntry) => void; onOpen: (e: UsageEntry) => void; onMenu: (e: UsageEntry, x: number, y: number) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [d, setD] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setD({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el); setD({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const tiles = useMemo(() => {
    const r = { x: 0, y: 0, w: d.w, h: d.h };
    return squarify(tileItems(data, r), r);
  }, [data, d.w, d.h]);
  const total = data.entries.reduce((a, e) => a + e.size, 0) + data.more_size;
  const selLow = sel?.toLowerCase();

  return (
    <div className={"su-tm" + (dim ? " dim" : "")} ref={box}>
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
            key={e.path} style={{ ...st, ...kc(e.kind) }}
            className={"su-tile" + (e.is_dir ? " dir" : "") + (e.path.toLowerCase() === selLow ? " on" : "")}
            title={`${e.name}\n${formatSize(e.size)} · ${formatPct(pct(e.size, total))}${e.is_dir ? ` · ${num(e.files)} files` : ""}`}
            tabIndex={-1}
            onClick={() => onSel(e)} onDoubleClick={() => onOpen(e)}
            onContextMenu={(ev) => { ev.preventDefault(); onSel(e); onMenu(e, ev.clientX, ev.clientY); }}
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
      {tiles.length === 0 && d.w > 0 && <div className="su-none">{data.entries.length === 0 ? "This folder is empty." : "Nothing in here takes up space."}</div>}
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

// ---- type / biggest-file lists ---------------------------------------------------------------------------------

function Types({ detail, total }: { detail: UsageDetail | null | undefined; total: number }) {
  if (detail === undefined) return <div className="su-none"><span className="spin" aria-label="Loading" /></div>;
  if (detail === null || detail.types.length === 0) return <div className="su-none">No files here.</div>;
  return (
    <div>
      {detail.types.map((t: UsageType) => {
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

function Biggest({ backend, detail, base, sel, onSel, onOpen, onMenu }: {
  backend: Backend; detail: UsageDetail | null | undefined; base: string; sel: string | null;
  onSel: (p: string) => void; onOpen: (h: Hit) => void; onMenu: (h: Hit, x: number, y: number) => void;
}) {
  if (detail === undefined) return <div className="su-none"><span className="spin" aria-label="Loading" /></div>;
  if (detail === null || detail.biggest.length === 0) return <div className="su-none">No files here.</div>;
  const cut0 = base.replace(/\\$/, "").length + 1;
  return (
    <div role="listbox" aria-label="Biggest files">
      {detail.biggest.map((h) => {
        const rel = h.path.slice(cut0);
        const cut = rel.lastIndexOf("\\");
        return (
          <div
            key={h.path} role="option" aria-selected={sel === h.path}
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
  if (!path) return <div className="su-sel idle">Select a folder or file to see what you can do with it.</div>;
  const name = entry?.name ?? hit!.name;
  const size = entry?.size ?? hit!.size ?? 0;
  const mtime = entry?.mtime ?? hit?.mtime ?? null;
  const isDir = !!entry?.is_dir;
  const meta = [
    formatSize(size) || "0 B",
    total > 0 && path.toLowerCase() !== "" ? `${formatPct(pct(size, total))} of this folder` : null,
    isDir ? `${num(entry!.files)} files` : null,
    mtime ? `Modified ${formatDate(mtime)}` : null,
  ].filter(Boolean).join(" · ");
  const isRoot = /^[A-Za-z]:\\?$/.test(path);
  return (
    <div className="su-sel">
      <div className="su-st"><b title={name}>{name}</b><small title={path}>{path}</small><small>{meta}</small></div>
      <div className="su-sb">
        {isDir && <button className="btn sm" onClick={() => onEnter(path)}>Open here</button>}
        {hit && <button className="btn sm" onClick={() => onShow(hit)}>Show in folder</button>}
        {!isDir && <button className="btn sm" onClick={() => actions.open(path)}>Open</button>}
        <button className="btn sm" onClick={() => actions.reveal(path)}>Reveal</button>
        {!isRoot && <button className="btn sm su-del" onClick={() => onRecycle(path)} title="Move to Recycle Bin (Delete)">Recycle</button>}
      </div>
    </div>
  );
}
