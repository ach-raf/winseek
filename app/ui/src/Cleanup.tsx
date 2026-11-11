import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, DupGroup, DupMember, Hit, NearState } from "./types";
import type { LibActions } from "./Gallery";
import { FileIcon } from "./FileIcon";
import { baseName } from "./Semantic";
import { formatDate, formatSize, splitDir } from "./format";
import { freedBytes, pickKeeper, totalReclaimable, uniformRange } from "./libraryLogic";
import { IChevL, ICheck } from "./icons";

type View = "home" | "exact" | "similar" | "large";
const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
const NEAR_THRESHOLD = 0.95;
const BIG_N = 200;
const HOME_ROWS = 6;

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

const dirName = (p: string) => {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length >= 2 ? parts[parts.length - 2] : parts[0] ?? "";
};

export function Cleanup({ backend, hidden, actions, epoch, gen, onError }: {
  backend: Backend;
  hidden: ReadonlySet<string>;
  actions: LibActions;
  epoch: number;
  /** Bumps when new embeddings arrived. */
  gen: number;
  onError: (e: unknown) => void;
}) {
  const [view, setView] = useState<View>("home");
  const [openGroup, setOpenGroup] = useState<string | null>(null);
  const [groups, setGroups] = useState<DupGroup[] | null>(null);
  const [files, setFiles] = useState<Hit[] | null>(null);

  useEffect(() => {
    let live = true;
    backend.libraryDupGroups().then((g) => { if (live) setGroups(g); }).catch((e) => { if (live) { setGroups([]); onError(e); } });
    return () => { live = false; };
  }, [backend, epoch, gen, onError]);
  useEffect(() => {
    let live = true;
    backend.libraryBigFiles(BIG_N).then((r) => { if (live) setFiles(r); }).catch((e) => { if (live) { setFiles([]); onError(e); } });
    return () => { live = false; };
  }, [backend, epoch, onError]);

  const vis = useMemo(() => (groups ? visibleGroups(groups, hidden) : null), [groups, hidden]);
  const rows = useMemo(() => (files ? files.filter((f) => !hidden.has(f.path)) : null), [files, hidden]);

  // Esc inside a drill-in goes back to the overview (the window-level handler in Library yields to a handled key).
  useEffect(() => {
    if (view === "home") return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.defaultPrevented || (t && /^(INPUT|TEXTAREA)$/.test(t.tagName))) return;
      if (e.key === "Escape" || (e.altKey && e.key === "ArrowLeft")) { e.preventDefault(); setView("home"); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view]);

  const home = () => setView("home");
  const bar = (title: string, summary: string, extra?: React.ReactNode) => (
    <div className="cl-sub">
      <button className="btn sm" onClick={home} title="Back to Clean up (Esc)"><IChevL size={14} />Clean up</button>
      <b>{title}</b>
      <span className="cl-sumtxt">{summary}</span>
      <span className="sp" />
      {extra}
    </div>
  );

  if (view === "exact") {
    return (
      <div className="cl">
        <GroupReview
          backend={backend} groups={vis} actions={actions} kind="exact" initialId={openGroup}
          header={(g) => bar("Duplicates", `${g.length.toLocaleString("en-US")} group${g.length === 1 ? "" : "s"} of identical files · ${formatSize(totalReclaimable(g))} can be freed`)}
          empty={<><strong>No exact duplicates</strong><span>Files with identical content show up here, once they are indexed.</span></>}
          emptyBar={bar("Duplicates", "")}
        />
      </div>
    );
  }
  if (view === "similar") {
    return <div className="cl"><Similar backend={backend} hidden={hidden} actions={actions} epoch={epoch} onError={onError} bar={bar} /></div>;
  }
  if (view === "large") {
    return <div className="cl"><Large backend={backend} rows={rows} actions={actions} bar={bar} /></div>;
  }
  return (
    <Home
      backend={backend} groups={vis} rows={rows} actions={actions}
      onReview={(id) => { setOpenGroup(id); setView("exact"); }}
      onSimilar={() => setView("similar")}
      onLarge={() => setView("large")}
    />
  );
}

// ---- overview: summary, two cards, floating selection bar ---------------------------------------------------

interface PickRow {
  key: string;
  name: string;
  sub: string;
  right: string;
  icon: React.ReactNode;
  path: string;
}

function Home({ backend, groups, rows, actions, onReview, onSimilar, onLarge }: {
  backend: Backend;
  groups: DupGroup[] | null;
  rows: Hit[] | null;
  actions: LibActions;
  onReview: (id: string) => void;
  onSimilar: () => void;
  onLarge: () => void;
}) {
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const dupInfo = useMemo(() => {
    const m = new Map<string, { keep: number; doomed: string[]; freed: number }>();
    for (const g of groups ?? []) {
      const keep = pickKeeper(g.members.map((x) => ({ ...x, width: dimCache.get(x.path)?.[0] ?? null, height: dimCache.get(x.path)?.[1] ?? null })));
      m.set(g.id, {
        keep,
        doomed: g.members.filter((_, i) => i !== keep).map((x) => x.path),
        freed: freedBytes(g.members, keep),
      });
    }
    return m;
  }, [groups]);

  const dupRows: PickRow[] = useMemo(() => (groups ?? []).slice(0, HOME_ROWS).map((g) => {
    const info = dupInfo.get(g.id)!;
    const keeper = g.members[info.keep];
    const others = Array.from(new Set(g.members.filter((_, i) => i !== info.keep).map((x) => dirName(x.path)))).slice(0, 2);
    return {
      key: "d:" + g.id,
      name: baseName(keeper.path),
      sub: others.length ? `Also in ${others.join(" and ")}` : "Identical copies",
      right: `${g.members.length} copies · ${formatSize(Math.max(...g.members.map((x) => x.size)))}`,
      icon: <GrThumb backend={backend} m={keeper} size={96} cls="ic" />,
      path: keeper.path,
    };
  }), [groups, dupInfo, backend]);

  const bigRows: PickRow[] = useMemo(() => (rows ?? []).slice(0, HOME_ROWS).map((f) => ({
    key: "b:" + f.path,
    name: f.name,
    sub: `${f.mtime ? `Modified ${formatDate(f.mtime)} · ` : ""}${dirName(f.path)}`,
    right: formatSize(f.size),
    icon: <span className="ic"><FileIcon name={f.name} isDir={false} src={backend.iconUrl(f.path, false, false)} /></span>,
    path: f.path,
  })), [rows, backend]);

  // What a pick means: a duplicates row = everything but the kept copy; a large-file row = that file.
  const live = useMemo(() => {
    const paths: string[] = [];
    let freed = 0, n = 0;
    for (const g of groups ?? []) {
      if (!picked.has("d:" + g.id)) continue;
      const i = dupInfo.get(g.id)!;
      paths.push(...i.doomed);
      freed += i.freed;
      n++;
    }
    for (const f of rows ?? []) {
      if (!picked.has("b:" + f.path)) continue;
      paths.push(f.path);
      freed += f.size ?? 0;
      n++;
    }
    return { paths, freed, n };
  }, [picked, groups, rows, dupInfo]);

  const toggle = useCallback((key: string) => setPicked((s) => { const n = new Set(s); if (n.has(key)) n.delete(key); else n.add(key); return n; }), []);

  const go = async () => {
    if (live.paths.length === 0) return;
    setBusy(true);
    try { await actions.recycle(live.paths); } finally { setBusy(false); }
  };

  const reclaim = groups ? totalReclaimable(groups) : null;
  const bigTotal = rows ? rows.reduce((a, f) => a + (f.size ?? 0), 0) : null;
  const num = (n: number | null) => (n === null ? "…" : formatSize(n));

  return (
    <div className="cl">
      <div className="page-body">
        <div className="sum">
          <div className="card hl"><small>You can free up</small><b>{reclaim === null || bigTotal === null ? "…" : formatSize(reclaim + bigTotal)}</b></div>
          <div className="card"><small>Duplicate files</small><b>{num(reclaim)}</b></div>
          <div className="card"><small>Large files you don't open</small><b>{num(bigTotal)}</b></div>
        </div>
        <div className="two">
          <section className="card">
            <h4>Duplicates<span>Keeps the original</span></h4>
            {groups === null ? <div className="cl-none"><span className="spin" aria-label="Loading" /></div>
              : dupRows.length === 0 ? <div className="cl-none">No exact duplicates. Identical files show up here once they are indexed.</div>
              : (
                <PickList
                  label="Duplicate groups" rows={dupRows} picked={picked} autoFocus
                  onToggle={toggle}
                  onOpen={(r) => onReview(r.key.slice(2))}
                  onDelete={(r) => { const i = dupInfo.get(r.key.slice(2)); if (i) void actions.recycle(i.doomed); }}
                />
              )}
            <div className="cl-foot">
              {groups !== null && groups.length > 0 && <button className="btn sm" onClick={() => onReview(groups[0].id)}>Review {groups.length.toLocaleString("en-US")} group{groups.length === 1 ? "" : "s"}</button>}
              <button className="btn sm" onClick={onSimilar}>Find similar photos</button>
            </div>
          </section>
          <section className="card">
            <h4>Large and untouched<span>Biggest first</span></h4>
            {rows === null ? <div className="cl-none"><span className="spin" aria-label="Loading" /></div>
              : bigRows.length === 0 ? <div className="cl-none">No large files to show. The list comes from the name service; it may not be running.</div>
              : (
                <PickList
                  label="Large files" rows={bigRows} picked={picked}
                  onToggle={toggle}
                  onOpen={(r) => actions.open(r.path)}
                  onDelete={(r) => void actions.recycle([r.path])}
                  onMenu={(r, x, y) => actions.menu(r.path, x, y)}
                />
              )}
            <div className="cl-foot">
              {rows !== null && rows.length > HOME_ROWS && <button className="btn sm" onClick={onLarge}>See all {rows.length.toLocaleString("en-US")}</button>}
              {rows !== null && rows.length > 0 && rows.length <= HOME_ROWS && <button className="btn sm" onClick={onLarge}>Open list</button>}
            </div>
          </section>
        </div>
      </div>
      {live.n > 0 && (
        <div className="selbar" role="region" aria-label="Selection">
          <span>{live.n} selected · frees <b>{formatSize(live.freed)}</b></span>
          <button className="btn" disabled={busy} onClick={() => void go()}>Move to Recycle Bin</button>
        </div>
      )}
    </div>
  );
}

/** A keyboard list of check rows: arrows move, Space picks, Enter opens, Delete recycles the focused row. */
function PickList({ rows, picked, onToggle, onOpen, onDelete, onMenu, label, autoFocus }: {
  rows: PickRow[];
  picked: ReadonlySet<string>;
  onToggle: (key: string) => void;
  onOpen: (r: PickRow) => void;
  onDelete: (r: PickRow) => void;
  onMenu?: (r: PickRow, x: number, y: number) => void;
  label: string;
  autoFocus?: boolean;
}) {
  const [act, setAct] = useState(0);
  const i = Math.min(act, rows.length - 1);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const r = rows[i];
    if (e.key === "ArrowDown") { e.preventDefault(); setAct(Math.min(rows.length - 1, i + 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setAct(Math.max(0, i - 1)); }
    else if (e.key === "Home") { e.preventDefault(); setAct(0); }
    else if (e.key === "End") { e.preventDefault(); setAct(rows.length - 1); }
    else if (e.key === " " && r) { e.preventDefault(); onToggle(r.key); }
    else if (e.key === "Enter" && r) { e.preventDefault(); onOpen(r); }
    else if (e.key === "Delete" && r) { e.preventDefault(); onDelete(r); }
  };
  return (
    <div className="pl" role="listbox" aria-multiselectable="true" aria-label={label} tabIndex={0} data-autofocus={autoFocus ? "" : undefined} onKeyDown={onKey}>
      {rows.map((r, k) => {
        const on = picked.has(r.key);
        return (
          <div
            key={r.key} role="option" aria-selected={on}
            className={"crow" + (k === i ? " cur" : "")}
            onClick={() => { setAct(k); onToggle(r.key); }}
            onDoubleClick={() => onOpen(r)}
            onContextMenu={onMenu ? (e) => { e.preventDefault(); setAct(k); onMenu(r, e.clientX, e.clientY); } : undefined}
          >
            <span className={"ck" + (on ? " on" : "")}><ICheck /></span>
            {r.icon}
            <div className="cn"><div className="n" title={r.path}>{r.name}</div><small>{r.sub}</small></div>
            <small>{r.right}</small>
          </div>
        );
      })}
    </div>
  );
}

// ---- near-duplicates (background scan) ----------------------------------------------------------------------

function Similar({ backend, hidden, actions, epoch, onError, bar }: {
  backend: Backend; hidden: ReadonlySet<string>; actions: LibActions; epoch: number; onError: (e: unknown) => void;
  bar: (title: string, summary: string, extra?: React.ReactNode) => React.ReactNode;
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
      <>
        {bar("Similar photos", "")}
        <div className="lg-empty cl-progress" role="status">
          <strong>Looking for similar photos…</strong>
          <div className="cl-bar"><i style={{ width: `${pct}%` }} /></div>
          <span>{near && near.total > 0 ? `${near.done.toLocaleString("en-US")} of ${near.total.toLocaleString("en-US")} photos compared` : "Starting…"} · runs in the background</span>
        </div>
      </>
    );
  }
  if (near.state === "error") {
    return (
      <>
        {bar("Similar photos", "")}
        <div className="lg-empty"><strong>The scan stopped</strong><span>{near.error}</span><button className="btn pri" onClick={start}>Try again</button></div>
      </>
    );
  }
  const rescan = <button className="btn sm" onClick={start} title="Scan again with the latest embeddings">Rescan</button>;
  return (
    <GroupReview
      backend={backend} groups={vis} actions={actions} kind="near" initialId={null}
      header={(g) => bar("Similar photos", `${g.length.toLocaleString("en-US")} group${g.length === 1 ? "" : "s"} of near-identical photos · cosine ≥ ${NEAR_THRESHOLD}`, rescan)}
      emptyBar={bar("Similar photos", "", rescan)}
      empty={<><strong>No similar photos found</strong><span>Bursts and re-saved copies show up here once photos are indexed.</span></>}
    />
  );
}

// ---- group list + side-by-side review ------------------------------------------------------------------------

const GROUP_H = 76;

function GroupReview({ backend, groups, actions, kind, header, emptyBar, empty, initialId }: {
  backend: Backend;
  groups: DupGroup[] | null;
  actions: LibActions;
  kind: "exact" | "near";
  header: (g: DupGroup[]) => React.ReactNode;
  emptyBar: React.ReactNode;
  empty: React.ReactNode;
  initialId: string | null;
}) {
  const [selId, setSelId] = useState<string | null>(initialId);
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
  }, [groups === null || groups.length === 0]);

  // Arriving with a group picked on the overview: scroll it into view once.
  const jumped = useRef(false);
  useEffect(() => {
    if (jumped.current || !groups || !initialId) return;
    const n = groups.findIndex((g) => g.id === initialId);
    if (n < 0) return;
    jumped.current = true;
    const el = list.current;
    if (el) el.scrollTop = Math.max(0, n * GROUP_H - GROUP_H);
  }, [groups, initialId]);

  if (groups === null) return <>{emptyBar}<div className="lg-empty"><span className="spin" aria-label="Loading" /></div></>;
  if (groups.length === 0) return <>{emptyBar}<div className="lg-empty">{empty}</div></>;

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
    <>
      {header(groups)}
      <div className="gr">
        <aside className="gr-side">
          <div className="gr-list" ref={list} tabIndex={0} data-autofocus onKeyDown={onKey} onScroll={(e) => setTop(e.currentTarget.scrollTop)} role="listbox" aria-label="Groups">
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
    </>
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

  // The review strip is a keyboard surface too (Tab reaches it from the group list): arrows move,
  // Space flips the focused file between keep and recycle, Delete does what the big button does.
  const [ci, setCi] = useState(() => Math.min(1, members.length - 1)); // start on the first compare card
  useEffect(() => { setCi((i) => Math.max(0, Math.min(i, members.length - 1))); }, [members.length]);
  const stripKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowRight" || e.key === "ArrowDown") { e.preventDefault(); setCi((i) => Math.min(cand.length - 1, i + 1)); }
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") { e.preventDefault(); setCi((i) => Math.max(0, i - 1)); }
    else if (e.key === " ") {
      e.preventDefault();
      const c = cand[ci];
      if (c && ci !== keep) toggle(c.path);
    } else if (e.key === "Enter" && cand[ci]) {
      e.preventDefault();
      actions.open(cand[ci].path);
    } else if (e.key === "Delete") {
      e.preventDefault();
      void go();
    }
  };

  return (
    <section className="gd">
      <div className="gd-cmp">
        <Pane backend={backend} c={cand[keep]} role="Keep" badge={keep === auto ? "Suggested" : "Your choice"} other={cand[other]} actions={actions} />
        {other >= 0 && <Pane backend={backend} c={cand[other]} role="Compare" badge={extraKeep.has(cand[other].path) ? "Kept too" : "To Recycle Bin"} other={cand[keep]} actions={actions} />}
      </div>
      <div className="gd-strip" role="list" tabIndex={0} onKeyDown={stripKey} aria-label="Files in this group">
        {cand.map((c, i) => (
          <div key={c.path} role="listitem" className={"gd-card" + (i === keep ? " keep" : i === other ? " cmp" : "") + (i === ci ? " kbf" : "")}
               onClick={() => { setCi(i); if (i !== keep) setBPath(c.path); }}>
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
        <span className="sp" />
        <button className="btn pri" disabled={busy || doomed.length === 0} onClick={() => void go()}>
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
          <button className="btn sm" onClick={() => actions.open(c.path)}>Open</button>
          <button className="btn sm" onClick={() => actions.reveal(c.path)}>Reveal</button>
        </div>
      </div>
    </figure>
  );
}

// ---- large files (full list) ----------------------------------------------------------------------------------

const BIG_H = 58;

function Large({ backend, rows, actions, bar }: {
  backend: Backend; rows: Hit[] | null; actions: LibActions;
  bar: (title: string, summary: string, extra?: React.ReactNode) => React.ReactNode;
}) {
  const [sel, setSel] = useState(0);
  const [top, setTop] = useState(0);
  const [h, setH] = useState(500);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
  }, [rows === null || rows.length === 0]);
  const n = rows?.length ?? 0;
  useEffect(() => { setSel((s) => Math.max(0, Math.min(s, n - 1))); }, [n]);

  if (rows === null) return <>{bar("Large and untouched", "")}<div className="lg-empty"><span className="spin" aria-label="Loading" /></div></>;
  if (rows.length === 0) {
    return <>{bar("Large and untouched", "")}<div className="lg-empty"><strong>No large files to show</strong><span>The list comes from the name service; it may not be running.</span></div></>;
  }
  const total = rows.reduce((a, f) => a + (f.size ?? 0), 0);
  const [from, to] = uniformRange(rows.length, BIG_H, top, h);
  const cur = rows[sel];

  const select = (i: number) => {
    setSel(i);
    const el = box.current;
    if (!el) return;
    if (i * BIG_H < el.scrollTop) el.scrollTop = i * BIG_H;
    else if ((i + 1) * BIG_H > el.scrollTop + el.clientHeight) el.scrollTop = (i + 1) * BIG_H - el.clientHeight;
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); select(Math.max(0, Math.min(rows.length - 1, sel + (e.key === "ArrowDown" ? 1 : -1)))); }
    else if (e.key === "Enter" && cur) { e.preventDefault(); if (e.ctrlKey) actions.reveal(cur.path); else actions.open(cur.path); }
    else if (e.key === "Delete" && cur) { e.preventDefault(); actions.recycle([cur.path]); }
  };

  return (
    <>
      {bar("Large and untouched", `The ${rows.length} largest files in your profile · ${formatSize(total)} together`)}
      <div className="bg">
        <div className="bg-list" ref={box} tabIndex={0} data-autofocus onKeyDown={onKey} onScroll={(e) => setTop(e.currentTarget.scrollTop)} role="listbox" aria-label="Large files">
          <div style={{ height: rows.length * BIG_H, position: "relative" }}>
            {rows.slice(from, to).map((f, k) => {
              const i = from + k;
              const { head, tail } = splitDir(f.path);
              return (
                <div key={f.path} role="option" aria-selected={i === sel} className={"bg-row" + (i === sel ? " on" : "")} style={{ top: i * BIG_H, height: BIG_H }}
                     onClick={() => setSel(i)} onDoubleClick={() => actions.open(f.path)}
                     onContextMenu={(e) => { e.preventDefault(); setSel(i); actions.menu(f.path, e.clientX, e.clientY); }}>
                  <span className="ic"><FileIcon name={f.name} isDir={false} src={backend.iconUrl(f.path, false, false)} /></span>
                  <span className="bg-name"><b title={f.name}>{f.name}</b><span className="bg-path" title={f.path}>{head}{tail}</span></span>
                  <span className="bg-size">{formatSize(f.size)}</span>
                  <span className="bg-date">{formatDate(f.mtime)}</span>
                  <button className="btn sm" tabIndex={-1} onClick={(e) => { e.stopPropagation(); actions.reveal(f.path); }}>Reveal</button>
                  <button className="btn sm" tabIndex={-1} onClick={(e) => { e.stopPropagation(); actions.recycle([f.path]); }}>Recycle</button>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </>
  );
}
