import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, DupGroup, DupMember, Hit, NearState } from "./types";
import type { LibActions } from "./Gallery";
import { FileIcon } from "./FileIcon";
import { baseName } from "./Semantic";
import { formatDate, formatSize, splitDir } from "./format";
import { pickKeeper, totalReclaimable, uniformRange } from "./libraryLogic";

type Mode = "exact" | "similar" | "large";
const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const NEAR_THRESHOLD = 0.95;
const BIG_N = 200;

export function Cleanup({ backend, hidden, actions, epoch, gen, onError }: {
  backend: Backend;
  hidden: ReadonlySet<string>;
  actions: LibActions;
  epoch: number;
  /** Bumps when new embeddings arrived. */
  gen: number;
  onError: (e: unknown) => void;
}) {
  const [mode, setMode] = useState<Mode>("exact");
  return (
    <div className="cl">
      <div className="cl-modes" role="tablist" aria-label="Clean up">
        {([["exact", "Exact duplicates"], ["similar", "Similar photos"], ["large", "Large files"]] as [Mode, string][]).map(([id, label]) => (
          <button key={id} role="tab" aria-selected={mode === id} className={"chip" + (mode === id ? " on" : "")} onClick={() => setMode(id)}>{label}</button>
        ))}
      </div>
      <div className="cl-body">
        {mode === "exact" && <Exact backend={backend} hidden={hidden} actions={actions} epoch={epoch} gen={gen} onError={onError} />}
        {mode === "similar" && <Similar backend={backend} hidden={hidden} actions={actions} epoch={epoch} onError={onError} />}
        {mode === "large" && <Large backend={backend} hidden={hidden} actions={actions} epoch={epoch} onError={onError} />}
      </div>
    </div>
  );
}

/** Groups minus what was recycled this session; a group with fewer than two files left disappears. */
function visibleGroups(groups: DupGroup[], hidden: ReadonlySet<string>): DupGroup[] {
  const out: DupGroup[] = [];
  for (const g of groups) {
    const members = g.members.filter((m) => !hidden.has(m.path));
    if (members.length < 2) continue;
    const total = members.reduce((a, m) => a + m.size, 0);
    out.push({ ...g, members, reclaimable: total - Math.max(...members.map((m) => m.size)) });
  }
  return out;
}

// ---- exact duplicates ---------------------------------------------------------------------------------------

function Exact({ backend, hidden, actions, epoch, gen, onError }: {
  backend: Backend; hidden: ReadonlySet<string>; actions: LibActions; epoch: number; gen: number; onError: (e: unknown) => void;
}) {
  const [groups, setGroups] = useState<DupGroup[] | null>(null);
  useEffect(() => {
    let live = true;
    backend.libraryDupGroups().then((g) => { if (live) setGroups(g); }).catch((e) => { if (live) { setGroups([]); onError(e); } });
    return () => { live = false; };
  }, [backend, epoch, gen, onError]);
  const vis = useMemo(() => (groups ? visibleGroups(groups, hidden) : null), [groups, hidden]);
  return (
    <GroupReview
      backend={backend} groups={vis} actions={actions} kind="exact"
      summary={(g) => `${g.length.toLocaleString("en-US")} group${g.length === 1 ? "" : "s"} of identical files · ${formatSize(totalReclaimable(g))} can be freed`}
      empty={<><strong>No exact duplicates</strong><span>Files with identical content show up here, once they are indexed.</span></>}
    />
  );
}

// ---- near-duplicates (background scan) ----------------------------------------------------------------------

function Similar({ backend, hidden, actions, epoch, onError }: {
  backend: Backend; hidden: ReadonlySet<string>; actions: LibActions; epoch: number; onError: (e: unknown) => void;
}) {
  const [near, setNear] = useState<NearState | null>(null);
  const started = useRef(false);

  const start = useCallback(() => {
    setNear({ state: "running", done: 0, total: 0, groups: [], error: null });
    backend.libraryNearStart(NEAR_THRESHOLD).catch((e) => { onError(e); setNear({ state: "error", done: 0, total: 0, groups: [], error: errText(e) }); });
  }, [backend, onError]);

  // First visit (or after an undo): reuse a finished scan, else start one.
  useEffect(() => {
    let live = true;
    backend.libraryNearPoll().then((s) => {
      if (!live) return;
      if (s.state === "idle" && !started.current) { started.current = true; start(); } else setNear(s);
    }).catch(onError);
    return () => { live = false; };
  }, [backend, epoch, start, onError]);

  // Poll while it runs.
  const running = near?.state === "running";
  useEffect(() => {
    if (!running) return;
    let live = true;
    const t = window.setInterval(() => {
      backend.libraryNearPoll().then((s) => { if (live && s.state !== "idle") setNear(s); }).catch(() => {});
    }, 500);
    return () => { live = false; window.clearInterval(t); };
  }, [backend, running]);

  const vis = useMemo(() => (near?.state === "done" ? visibleGroups(near.groups, hidden) : null), [near, hidden]);
  const pct = near && near.total > 0 ? Math.round((near.done / near.total) * 100) : 0;

  if (!near || near.state === "running") {
    return (
      <div className="lg-empty cl-progress" role="status">
        <strong>Looking for similar photos…</strong>
        <div className="cl-bar"><i style={{ width: `${pct}%` }} /></div>
        <span>{near && near.total > 0 ? `${near.done.toLocaleString("en-US")} of ${near.total.toLocaleString("en-US")} photos compared` : "Starting…"} · runs in the background</span>
      </div>
    );
  }
  if (near.state === "error") {
    return (
      <div className="lg-empty"><strong>The scan stopped</strong><span>{near.error}</span><button className="btn primary" onClick={start}>Try again</button></div>
    );
  }
  return (
    <GroupReview
      backend={backend} groups={vis} actions={actions} kind="near"
      summary={(g) => `${g.length.toLocaleString("en-US")} group${g.length === 1 ? "" : "s"} of near-identical photos · cosine ≥ ${NEAR_THRESHOLD}`}
      extra={<button className="btn quiet" onClick={start} title="Scan again with the latest embeddings">Rescan</button>}
      empty={<><strong>No similar photos found</strong><span>Bursts and re-saved copies show up here once photos are indexed.</span></>}
    />
  );
}

// ---- group list + side-by-side review ------------------------------------------------------------------------

const GROUP_H = 76;

function GroupReview({ backend, groups, actions, kind, summary, empty, extra }: {
  backend: Backend;
  groups: DupGroup[] | null;
  actions: LibActions;
  kind: "exact" | "near";
  summary: (g: DupGroup[]) => string;
  empty: React.ReactNode;
  extra?: React.ReactNode;
}) {
  const [selId, setSelId] = useState<string | null>(null);
  const [top, setTop] = useState(0);
  const [h, setH] = useState(500);
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = list.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
  }, [groups === null]);

  if (groups === null) return <div className="lg-empty"><span className="spin" aria-label="Loading" /></div>;
  if (groups.length === 0) return <div className="lg-empty">{empty}{extra}</div>;

  const idx = Math.max(0, groups.findIndex((g) => g.id === selId));
  const sel = groups[idx];
  const [from, to] = uniformRange(groups.length, GROUP_H, top, h);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const n = Math.max(0, Math.min(groups.length - 1, idx + (e.key === "ArrowDown" ? 1 : -1)));
    setSelId(groups[n].id);
    const el = list.current;
    if (el) {
      if (n * GROUP_H < el.scrollTop) el.scrollTop = n * GROUP_H;
      else if ((n + 1) * GROUP_H > el.scrollTop + el.clientHeight) el.scrollTop = (n + 1) * GROUP_H - el.clientHeight;
    }
  };

  return (
    <div className="gr">
      <aside className="gr-side">
        <div className="gr-sum"><span>{summary(groups)}</span>{extra}</div>
        <div className="gr-list" ref={list} tabIndex={0} onKeyDown={onKey} onScroll={(e) => setTop(e.currentTarget.scrollTop)} role="listbox" aria-label="Groups">
          <div style={{ height: groups.length * GROUP_H, position: "relative" }}>
            {groups.slice(from, to).map((g, k) => {
              const i = from + k;
              return (
                <button key={g.id} role="option" aria-selected={i === idx} className={"gr-item" + (i === idx ? " on" : "")} style={{ top: i * GROUP_H, height: GROUP_H }} tabIndex={-1} onClick={() => setSelId(g.id)}>
                  <span className="gr-thumbs">
                    {g.members.slice(0, 4).map((m) => <GrThumb key={m.path} backend={backend} m={m} size={96} cls="gr-t" />)}
                    {g.members.length > 4 && <em>+{g.members.length - 4}</em>}
                  </span>
                  <span className="gr-meta">
                    <b>{g.members.length} {kind === "exact" ? "copies" : "similar photos"}</b>
                    <span>{formatSize(g.reclaimable)} to free · {baseName(g.members[0].path)}</span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </aside>
      <GroupDetail key={sel.id} backend={backend} group={sel} kind={kind} actions={actions} />
    </div>
  );
}

function GrThumb({ backend, m, size, cls }: { backend: Backend; m: DupMember; size: number; cls: string }) {
  const src = backend.iconUrl(m.path, false, true, size);
  const [bad, setBad] = useState(false);
  return src && !bad
    ? <img className={cls} src={src} alt="" draggable={false} decoding="async" onError={() => setBad(true)} />
    : <span className={cls + " ph"}><FileIcon name={baseName(m.path)} isDir={false} src={null} /></span>;
}

const dimCache = new Map<string, [number, number] | null>();

function GroupDetail({ backend, group, kind, actions }: { backend: Backend; group: DupGroup; kind: "exact" | "near"; actions: LibActions }) {
  const members = group.members;
  const [dims, setDims] = useState<Record<string, [number, number] | null>>({});
  const [keepPath, setKeepPath] = useState<string | null>(null);
  const [bPath, setBPath] = useState<string | null>(null);
  const [extraKeep, setExtraKeep] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const paths = members.map((m) => m.path).join("|");
  useEffect(() => {
    let live = true;
    const need = members.map((m) => m.path).filter((p) => !dimCache.has(p));
    const have = () => Object.fromEntries(members.map((m) => [m.path, dimCache.get(m.path) ?? null]));
    setDims(have());
    if (need.length === 0) return;
    backend.libraryDims(need).then((r) => {
      need.forEach((p, i) => dimCache.set(p, r[i] ?? null));
      if (live) setDims(have());
    }).catch(() => {});
    return () => { live = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend, paths]);

  const cand = members.map((m) => ({ ...m, width: dims[m.path]?.[0] ?? null, height: dims[m.path]?.[1] ?? null }));
  const auto = pickKeeper(cand);
  const kept = keepPath ? cand.findIndex((c) => c.path === keepPath) : -1;
  const keep = kept >= 0 ? kept : auto;
  const bi = bPath ? cand.findIndex((c) => c.path === bPath) : -1;
  const other = bi >= 0 && bi !== keep ? bi : cand.findIndex((_, i) => i !== keep);

  const doomed = cand.filter((c, i) => i !== keep && !extraKeep.has(c.path));
  const freed = doomed.reduce((a, c) => a + c.size, 0);

  const toggle = (p: string) => setExtraKeep((s) => { const n = new Set(s); if (n.has(p)) n.delete(p); else n.add(p); return n; });

  const go = async () => {
    setBusy(true);
    try { await actions.recycle(doomed.map((d) => d.path)); } finally { setBusy(false); }
  };

  return (
    <section className="gd">
      <div className="gd-cmp">
        <Pane backend={backend} c={cand[keep]} role="Keep" badge={keep === auto ? "Suggested" : "Your choice"} other={cand[other]} actions={actions} />
        {other >= 0 && <Pane backend={backend} c={cand[other]} role="Compare" badge={extraKeep.has(cand[other].path) ? "Kept too" : "To Recycle Bin"} other={cand[keep]} actions={actions} />}
      </div>
      <div className="gd-strip" role="list">
        {cand.map((c, i) => (
          <div key={c.path} role="listitem" className={"gd-card" + (i === keep ? " keep" : i === other ? " cmp" : "")} onClick={() => { if (i !== keep) setBPath(c.path); }}>
            <GrThumb backend={backend} m={c} size={192} cls="gd-th" />
            <div className="gd-cn" title={c.path}>{baseName(c.path)}</div>
            <div className="gd-cf">{formatSize(c.size)}{c.width && c.height ? ` · ${c.width}×${c.height}` : ""}</div>
            {kind === "near" && c.sim !== null && <div className="gd-cf">{(c.sim * 100).toFixed(1)}% alike</div>}
            {i === keep ? (
              <span className="gd-tag">Keeping{i === auto ? " (best)" : ""}</span>
            ) : (
              <div className="gd-act">
                <label onClick={(e) => e.stopPropagation()}><input type="checkbox" checked={!extraKeep.has(c.path)} onChange={() => toggle(c.path)} /> Recycle</label>
                <button className="gd-link" onClick={(e) => { e.stopPropagation(); setKeepPath(c.path); setBPath(null); }}>Keep this</button>
              </div>
            )}
          </div>
        ))}
      </div>
      <footer className="gd-bar">
        <span>Suggested keep: largest resolution, then newest, then shortest path.</span>
        <span className="spacer" />
        <button className="btn primary" disabled={busy || doomed.length === 0} onClick={() => void go()}>
          Move {doomed.length} to Recycle Bin{doomed.length ? ` · frees ${formatSize(freed)}` : ""}
        </button>
      </footer>
    </section>
  );
}

function Pane({ backend, c, role, badge, other, actions }: {
  backend: Backend; c: DupMember & { width: number | null; height: number | null }; role: string; badge: string;
  other: (DupMember & { width: number | null; height: number | null }) | undefined; actions: LibActions;
}) {
  const px = (x: { width: number | null; height: number | null } | undefined) => (x?.width ?? 0) * (x?.height ?? 0);
  const bigger = !!other && px(c) > px(other);
  const heavier = !!other && c.size > other.size;
  const newer = !!other && c.mtime > other.mtime;
  const { head, tail } = splitDir(c.path);
  const src = backend.iconUrl(c.path, false, true, 512);
  return (
    <figure className="gd-pane">
      <figcaption><b>{role}</b><span className="gd-badge">{badge}</span></figcaption>
      <div className="gd-img">{src ? <img src={src} alt={baseName(c.path)} draggable={false} /> : <FileIcon name={baseName(c.path)} isDir={false} src={null} />}</div>
      <div className="gd-facts">
        <div className="gd-fn" title={c.path}>{baseName(c.path)}</div>
        <div className="gd-path" title={c.path}>{head}{tail}</div>
        <div className="gd-fl">
          <span className={bigger ? "better" : ""}>{c.width && c.height ? `${c.width} × ${c.height}` : "size unknown"}</span>
          <span className={heavier ? "better" : ""}>{formatSize(c.size)}</span>
          <span className={newer ? "better" : ""}>{formatDate(c.mtime)}</span>
        </div>
        <div className="gd-btns">
          <button className="btn quiet" onClick={() => actions.open(c.path)}>Open</button>
          <button className="btn quiet" onClick={() => actions.reveal(c.path)}>Reveal</button>
        </div>
      </div>
    </figure>
  );
}

// ---- large files ---------------------------------------------------------------------------------------------

const BIG_H = 54;

function Large({ backend, hidden, actions, epoch, onError }: {
  backend: Backend; hidden: ReadonlySet<string>; actions: LibActions; epoch: number; onError: (e: unknown) => void;
}) {
  const [files, setFiles] = useState<Hit[] | null>(null);
  const [sel, setSel] = useState(0);
  const [top, setTop] = useState(0);
  const [h, setH] = useState(500);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    backend.libraryBigFiles(BIG_N).then((r) => { if (live) setFiles(r); }).catch((e) => { if (live) { setFiles([]); onError(e); } });
    return () => { live = false; };
  }, [backend, epoch, onError]);

  const rows = useMemo(() => (files ?? []).filter((f) => !hidden.has(f.path)), [files, hidden]);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
  }, [files === null]);
  useEffect(() => { setSel((s) => Math.max(0, Math.min(s, rows.length - 1))); }, [rows.length]);

  if (files === null) return <div className="lg-empty"><span className="spin" aria-label="Loading" /></div>;
  if (rows.length === 0) {
    return <div className="lg-empty"><strong>No large files to show</strong><span>The list comes from the name service; it may not be running.</span></div>;
  }
  const total = rows.reduce((a, f) => a + (f.size ?? 0), 0);
  const [from, to] = uniformRange(rows.length, BIG_H, top, h);
  const cur = rows[sel];

  const select = (n: number) => {
    setSel(n);
    const el = box.current;
    if (!el) return;
    if (n * BIG_H < el.scrollTop) el.scrollTop = n * BIG_H;
    else if ((n + 1) * BIG_H > el.scrollTop + el.clientHeight) el.scrollTop = (n + 1) * BIG_H - el.clientHeight;
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); select(Math.max(0, Math.min(rows.length - 1, sel + (e.key === "ArrowDown" ? 1 : -1)))); }
    else if (e.key === "Enter" && cur) { e.preventDefault(); if (e.ctrlKey) actions.reveal(cur.path); else actions.open(cur.path); }
    else if (e.key === "Delete" && cur) { e.preventDefault(); actions.recycle([cur.path]); }
  };

  return (
    <div className="bg">
      <div className="gr-sum"><span>The {rows.length} largest files in your profile · {formatSize(total)} together</span></div>
      <div className="bg-list" ref={box} tabIndex={0} onKeyDown={onKey} onScroll={(e) => setTop(e.currentTarget.scrollTop)} role="listbox" aria-label="Large files">
        <div style={{ height: rows.length * BIG_H, position: "relative" }}>
          {rows.slice(from, to).map((f, k) => {
            const i = from + k;
            const { head, tail } = splitDir(f.path);
            return (
              <div key={f.path} role="option" aria-selected={i === sel} className={"bg-row" + (i === sel ? " on" : "")} style={{ top: i * BIG_H, height: BIG_H }}
                   onClick={() => setSel(i)} onDoubleClick={() => actions.open(f.path)}
                   onContextMenu={(e) => { e.preventDefault(); setSel(i); actions.menu(f.path, e.clientX, e.clientY); }}>
                <FileIcon name={f.name} isDir={false} src={backend.iconUrl(f.path, false, false)} />
                <span className="bg-name"><b title={f.name}>{f.name}</b><span className="bg-path" title={f.path}>{head}{tail}</span></span>
                <span className="bg-size">{formatSize(f.size)}</span>
                <span className="bg-date">{formatDate(f.mtime)}</span>
                <button className="btn quiet bg-b" tabIndex={-1} onClick={(e) => { e.stopPropagation(); actions.reveal(f.path); }}>Reveal</button>
                <button className="btn quiet bg-b" tabIndex={-1} onClick={(e) => { e.stopPropagation(); actions.recycle([f.path]); }}>Recycle</button>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
