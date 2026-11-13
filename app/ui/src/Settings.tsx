import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { AgentConfig, AgentLogEntry, Backend, Hotkeys, IgnoreList, LaunchCfg, LaunchCounts, McpPath, ModelGroup, Profile, StatusModels, Tier1Status, UpdateState } from "./types";
import type { Models } from "./hooks";
import { rowAction, type RowAction } from "./modelRows";
import { argsPreview, snippets } from "./agentLogic";
import { Bar, Models as ModelsPane, Names, StatusMeaning, useStatusPage, waitingText } from "./Status";
import { formatDate, formatDuration, formatPages, formatRate, formatSize } from "./format";
import { eventToCombo } from "./hotkeyLogic";
import { Folders, pausedText } from "./Setup";
import { trapKey } from "./trap";
import { matches } from "./keymap";
import { filterSections, type SettingsSection } from "./settingsNav";
import { ACCENTS, applyAccent, loadAccent, saveAccent, type AccentId } from "./accent";
import type { ViewMode } from "./viewMode";
import { Grp, Row, Seg, Switch, XIcon } from "./settingsUi";
import { IChevL, ISearch } from "./icons";

const fmt = (n: number) => n.toLocaleString("en-US");
const lc = (s: string) => s.toLowerCase();

/** Split pasted text into names: commas, semicolons and new lines separate entries. */
function parseNames(text: string): string[] {
  return text.split(/[,;\r\n]+/).map((s) => s.trim().replace(/^"(.*)"$/, "$1").replace(/^[\\/]+|[\\/]+$/g, "")).filter(Boolean);
}

/** Skipped names shown before "Show all". */
const IG_PEEK = 28;

function IgnoredNames({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [ig, setIg] = useState<IgnoreList | null>(null);
  const [filter, setFilter] = useState("");
  const [text, setText] = useState("");
  const [bad, setBad] = useState<string | null>(null);
  const [more, setMore] = useState(false); // the full list of names is opened on request (it never scrolls inside the card)
  useEffect(() => { backend.tier1GetIgnore().then(setIg).catch(onError); }, [backend, onError]);

  const save = useCallback((list: string[] | null) => { backend.tier1SetIgnore(list).then(setIg).catch(onError); }, [backend, onError]);
  const defaults = useMemo(() => new Set((ig?.defaults ?? []).map(lc)), [ig]);
  const isDefault = !!ig && ig.list.length === ig.defaults.length && ig.list.every((n) => defaults.has(lc(n)));
  const shown = useMemo(() => {
    const f = lc(filter.trim());
    return (ig?.list ?? []).filter((n) => !f || lc(n).includes(f));
  }, [ig, filter]);
  const removedDefaults = ig ? ig.defaults.filter((d) => !ig.list.some((n) => lc(n) === lc(d))).length : 0;

  const add = () => {
    if (!ig) return;
    const names = parseNames(text);
    if (!names.length) return;
    if (names.some((n) => /[\\/]/.test(n))) { setBad("Enter a folder or file name like \"out\", not a path."); return; }
    setBad(null);
    setText("");
    const have = new Set(ig.list.map(lc));
    const fresh = names.filter((n) => !have.has(lc(n)));
    if (fresh.length) save([...fresh, ...ig.list]);
  };
  const remove = (n: string) => ig && save(ig.list.filter((x) => x !== n));

  return (
    <Grp title="Skipped names" note={<>Skipped anywhere inside your folders. <code>*</code> and <code>?</code> are wildcards. Name search still finds them.</>}>
      <div className="grow addrow">
        <input className="gin" value={text} onChange={(e) => { setText(e.target.value); setBad(null); }} placeholder="Add names, e.g. Raw*, scratch" aria-label="Add ignored names"
               spellCheck={false} autoComplete="off"
               onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); add(); } }} />
        <button className="gbtn" onClick={add} disabled={!text.trim() || !ig}>Add</button>
      </div>
      {bad && <div className="grow"><span className="fr-error" role="alert">{bad}</span></div>}
      <div className="grow addrow">
        <input className="gin sm" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter" aria-label="Filter ignored names"
               spellCheck={false} autoComplete="off" />
        <span className="nums">
          {ig ? `${fmt(ig.list.length)} name${ig.list.length === 1 ? "" : "s"}` : "Loading…"}
          {ig && (isDefault ? " · defaults" : removedDefaults ? ` · ${fmt(removedDefaults)} default${removedDefaults === 1 ? "" : "s"} removed` : "")}
        </span>
        <button className="gbtn" onClick={() => save(null)} disabled={!ig || isDefault}>Reset to defaults</button>
      </div>
      <div className="grow stack">
        <ul className="ig-list" aria-label="Ignored names">
          {(more || filter.trim() ? shown : shown.slice(0, IG_PEEK)).map((n) => (
            <li key={n} className={defaults.has(lc(n)) ? "" : "custom"} title={defaults.has(lc(n)) ? n : `${n} (added by you)`}>
              <span>{n}</span>
              <button className="ig-x" onClick={() => remove(n)} aria-label={`Stop ignoring ${n}`} title="Remove"><XIcon size={10} /></button>
            </li>
          ))}
          {ig && shown.length === 0 && <li className="none">{ig.list.length ? "No name matches the filter" : "Nothing is ignored"}</li>}
          {!filter.trim() && shown.length > IG_PEEK && (
            <li className="none">
              <button className="ig-more" onClick={() => setMore((m) => !m)}>{more ? "Show fewer" : `Show all ${fmt(shown.length)}`}</button>
            </li>
          )}
        </ul>
      </div>
    </Grp>
  );
}

function CodeToggle({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => { backend.tier1GetIndexCode().then(setOn).catch(onError); }, [backend, onError]);
  const change = (v: boolean) => {
    setOn(v);
    backend.tier1SetIndexCode(v).then(setOn).catch((e) => { setOn(!v); onError(e); });
  };
  return (
    <Grp title="Source code">
      <Row title="Index source code" desc="Search inside code files by what they say. Files ignored by .gitignore are left out. Search them with the code: prefix, e.g. code: parse config; they never show up in other searches.">
        <Switch checked={!!on} disabled={on === null} onChange={change} label="Index source code" />
      </Row>
    </Grp>
  );
}

function AutostartRow({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => { backend.autostartGet().then(setOn).catch(onError); }, [backend, onError]);
  const change = (v: boolean) => {
    setOn(v);
    backend.autostartSet(v).then(setOn).catch((e) => { setOn(!v); onError(e); });
  };
  return (
    <Row title="Start with Windows" desc="Starts in the tray when you sign in, so the shortcut works right away">
      <Switch checked={!!on} disabled={on === null} onChange={change} label="Start with Windows" />
    </Row>
  );
}

/** Daily check for a newer release. Only tells you (tray + this row) and opens the release page; installs nothing. */
function UpdatesRow({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [u, setU] = useState<UpdateState | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    backend.updatesGet().then(setU).catch(onError);
    let off: (() => void) | undefined, dead = false;
    backend.onUpdateState(() => backend.updatesGet().then(setU).catch(onError)).then((f) => (dead ? f() : (off = f)));
    return () => { dead = true; off?.(); };
  }, [backend, onError]);
  const change = (v: boolean) => {
    setU((c) => (c ? { ...c, enabled: v } : c));
    backend.updatesSetEnabled(v).then(setU).catch((e) => { onError(e); backend.updatesGet().then(setU).catch(onError); });
  };
  const check = () => { setBusy(true); backend.updatesCheckNow().then(setU).catch(onError).finally(() => setBusy(false)); };
  const note = u?.latest
    ? `${u.latest} is available.`
    : u && u.last_check > 0 ? `Up to date (checked ${formatDate(u.last_check * 1000)}).` : "Not checked yet.";
  return (
    <Row title="Check for updates daily"
         desc={`Looks at the public GitHub releases once a day and tells you in the tray. It sends nothing about you or this PC and installs nothing. ${note}`}>
      {u?.latest && <button className="gbtn" onClick={() => backend.updatesOpenPage().catch(onError)}>Open release page</button>}
      <button className="gbtn" disabled={busy || !u} onClick={check}>{busy ? "Checking…" : "Check now"}</button>
      <Switch checked={!!u?.enabled} disabled={!u} onChange={change} label="Check for updates daily" />
    </Row>
  );
}

/** The app's own version (Settings › General). Hidden in the browser mock, which has no package info. */
function VersionRow({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [v, setV] = useState<string | null>(null);
  useEffect(() => { backend.appVersion().then(setV).catch(onError); }, [backend, onError]);
  if (!v) return null;
  return <Row title="Version" desc={`winseek for Windows ${v}`} hint={`winseek v${v}`}><span className="val">{v}</span></Row>;
}

/** Accent colour swatches (Settings › General). Applies at once and is remembered; the other
 *  window follows through the storage event (main.tsx). */
function AccentRow() {
  const [id, setId] = useState<AccentId>(() => loadAccent());
  const change = (next: AccentId) => {
    setId(next);
    applyAccent(next);
    saveAccent(next);
  };
  return (
    <Row title="Accent colour" desc="The highlight colour across winseek.">
      <div className="acc-swatches" role="radiogroup" aria-label="Accent colour">
        {ACCENTS.map((a) => (
          <button key={a.id} role="radio" aria-checked={id === a.id} aria-label={a.label}
                  title={a.label} className={"acc-sw" + (id === a.id ? " on" : "")}
                  style={{ background: a.hex }} onClick={() => change(a.id)} />
        ))}
      </div>
    </Row>
  );
}

const PROFILES: { id: Profile; label: string; blurb: string }[] = [
  { id: "low", label: "Light", blurb: "Least graphics memory. Slower on images and documents." },
  { id: "balanced", label: "Balanced", blurb: "Search runs on the processor; image and document models use the graphics card only while they work." },
  { id: "fast", label: "Fast", blurb: "Keeps models loaded while the app is open. Uses the most graphics memory." },
];

function Performance({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [p, setP] = useState<Profile | null>(null);
  useEffect(() => { backend.getProfile().then(setP).catch(onError); }, [backend, onError]);
  const change = (v: Profile) => {
    const before = p;
    setP(v);
    backend.setProfile(v).then(setP).catch((e) => { setP(before); onError(e); });
  };
  const shown = p ?? "balanced";
  return (
    <Grp title="Performance" note="Applies to search by meaning: where the models run and how long they stay loaded. Name search is not affected.">
      <Row title="Background work" desc={PROFILES.find((x) => x.id === shown)?.blurb}>
        <Seg value={shown} options={PROFILES} onChange={change} disabled={p === null} label="Performance profile" />
      </Row>
    </Grp>
  );
}

const VIEW_OPTIONS: { id: ViewMode; label: string }[] = [{ id: "compact", label: "Popup" }, { id: "full", label: "Full window" }];

/** Which view the global hotkey opens: the compact popup or the full-window layout. */
function DefaultViewRow({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [v, setV] = useState<ViewMode | null>(null);
  useEffect(() => { backend.viewGet().then((s) => setV(s.default_view)).catch(onError); }, [backend, onError]);
  const change = (m: ViewMode) => {
    const before = v;
    setV(m);
    backend.viewSetDefault(m).then((s) => setV(s.default_view)).catch((e) => { setV(before); onError(e); });
  };
  return (
    <Row title="Open as" desc="What the shortcut opens. F11 switches between the two at any time.">
      <Seg value={v ?? "compact"} options={VIEW_OPTIONS} onChange={change} disabled={v === null} label="Default view" />
    </Row>
  );
}

const engineName = (m: StatusModels) =>
  m.backend === "cpu" ? "the processor" : `${m.gpu ?? "the graphics card"} (${m.backend === "cuda12" ? "CUDA" : "Vulkan"})`;

/** One component: installed status, or what can be done about it (download, progress, retry). */
function ModelLine({ name, detail, act, optional, label, onGet, onCancel, children }: {
  name: string; detail?: string; act: RowAction;
  /** Missing is fine (pdfium arrives with the first PDF, the parser is an extra): no warning mark. */
  optional?: boolean;
  /** Button verb: "Download" or "Install". */
  label: string;
  onGet: () => void; onCancel: () => void; children: ReactNode;
}) {
  const ok = act.kind === "installed" ? true : optional ? null : false;
  const size = (b: number | null) => (b != null ? ` · ${formatSize(b)}` : "");
  const paused = act.kind === "failed" && act.error === "cancelled";
  const desc: ReactNode = act.kind === "installed" ? children
    : act.kind === "downloading" ? (act.total > 0 ? `${formatSize(act.got)} of ${formatSize(act.total)}` : "Starting…")
    : act.kind === "waiting" ? "Waiting for the current download"
    : act.kind === "failed" ? (paused ? "Download paused. Resume continues where it stopped." : <span className="bad" role="alert">{`Download failed: ${act.error}. Retry continues where it stopped.`}</span>)
    : children;
  return (
    <div className="grow wrap" role="listitem">
      <div className="gtxt">
        <b><i className={"mdot " + (ok === true ? "ok" : ok === false ? "miss" : "opt")} aria-hidden />{name}{detail && <em> · {detail}</em>}</b>
        <span>{desc}</span>
      </div>
      <div className="gctl">
        {act.kind === "downloading" ? <button className="gbtn" onClick={onCancel}>Cancel</button>
          : act.kind === "failed" ? <button className="gbtn" onClick={onGet}>{paused ? "Resume" : "Retry"}{size(act.bytes)}</button>
          : act.kind === "download" ? <button className="gbtn" onClick={onGet}>{label}{size(act.bytes)}</button>
          : null}
      </div>
      {act.kind === "downloading" && <div className="gsub"><Bar value={act.frac} label={`Downloading ${name}`} /></div>}
    </div>
  );
}

/** Search by meaning at a glance: what it is doing, how fast, and which models are installed / loaded. */
function Meaning({ backend, t1, models, onPause, onSetup }: {
  backend: Backend; t1: Tier1Status | null; models: Models; onPause: (p: boolean) => void; onSetup: () => void;
}) {
  const [page, , reload] = useStatusPage(backend);
  const { refreshPlan, running } = models;
  useEffect(() => { refreshPlan(); }, [refreshPlan]);
  // A download that ends (or fails) shows its result right away, not at the next status poll.
  const wasRunning = useRef(running);
  useEffect(() => { if (wasRunning.current && !running) reload(); wasRunning.current = running; }, [running, reload]);
  const st = page?.tier1;
  const m = page?.models;
  const s = st?.stats ?? t1?.stats;
  const off = !t1 || t1.state === "models_missing" || t1.state === "unavailable";
  const pr = st?.paused_reason ?? t1?.paused_reason ?? null;
  const total = s ? s.done + s.pending + s.text_only : 0;
  // Document-parser progress: the enrichment pass that runs once the embed queue is empty.
  const q = m?.parser.queue;
  const pTotal = q ? q.queued + q.done + q.failed + q.skipped : 0;
  const pLeft = q?.queued ?? 0;
  const pFailed = q?.failed ?? 0;
  const pRate = m?.parser.rate ?? null;
  const pEta = m?.parser.eta_s ?? null;
  const indexing = st?.state === "indexing" || t1?.state === "indexing";
  const reading = indexing && s != null && s.pending + s.text_only === 0 && pLeft > 0;
  const head = !t1 ? "Loading…"
    : t1.state === "unavailable" ? `Unavailable: ${t1.error ?? "unknown error"}`
    : t1.state === "models_missing" ? "Not set up. The model files are not downloaded yet."
    : pr ? pausedText(pr)
    : reading ? "Reading documents"
    : indexing ? "Indexing"
    : s && s.pending > 0 ? (st ? waitingText(st) : "Waiting for work") : "All caught up";
  const live = !off && !pr;
  const files = m?.files ?? [];
  const embedOk = files.length > 0 && files.every((f) => f.bytes != null && f.bytes === f.expected_bytes);
  const embedSize = files.reduce((a, f) => a + (f.bytes ?? 0), 0);
  const sv = m?.server;
  const act = (g: ModelGroup, installed: boolean) => rowAction(g, installed, models);
  const get = (g: ModelGroup) => () => models.start(g === "parser", [g]);
  return (
    <>
      <Grp title="Search by meaning" label="Search by meaning">
        <Row title="Indexing" desc={<span className={"st-head " + (live ? "s-live" : "s-warn")}><span className="dot" />{head}</span>}>
          {off ? (
            t1?.state === "models_missing" && <button className="gbtn pri" onClick={onSetup}>Set up…</button>
          ) : (
            (pr == null || pr === "user") && (
              <button className="gbtn" onClick={() => onPause(pr == null)}>{pr == null ? "Pause" : "Resume"}</button>
            )
          )}
        </Row>
        {s && total > 0 && (
          <div className="grow stack">
            <div className="idx-line">
              <span className="nums"><b>{fmt(s.done)} of {fmt(total)} files</b></span>
              <span className="nums">
                {[st?.rate != null && st.rate > 0 ? formatRate(st.rate) : null,
                  st?.eta_s != null && live ? `about ${formatDuration(st.eta_s)} left` : null].filter(Boolean).join(" · ")}
              </span>
            </div>
            <Bar value={total ? s.done / total : null} label="Files indexed" />
          </div>
        )}
        {m?.parser.present && pTotal > 0 && (pLeft > 0 || pRate != null) && (
          <div className="grow stack">
            <div className="idx-line">
              <span className="nums"><b>{fmt(pTotal - pLeft)} of {fmt(pTotal)} pages</b></span>
              <span className="nums">
                {[pRate != null ? formatPages(pRate) : null,
                  pRate != null && pEta != null && pLeft > 0 && !pr ? `about ${formatDuration(pEta)} left` : null].filter(Boolean).join(" · ")}
              </span>
            </div>
            <Bar value={pTotal ? (pTotal - pLeft) / pTotal : null} label="Pages read by the document parser" />
          </div>
        )}
      </Grp>
      {m && (
        <section className="grp" aria-label="Models">
          <h3>Components</h3>
          <div className="gcard" role="list">
            <ModelLine name={m.model_id} detail={`${m.quant}${embedSize ? `, ${formatSize(embedSize)}` : ""}`}
                       act={act("embedder", embedOk)} label="Download" onGet={get("embedder")} onCancel={models.cancel}>
              {!embedOk ? "Not downloaded"
                : sv?.running ? `Loaded on ${engineName(m)}`
                : `Installed · loads on ${engineName(m)} when there is work`}
            </ModelLine>
            <ModelLine name="llama.cpp" detail={m.llama_build}
                       act={act("llama", m.llama_present)} label="Download" onGet={get("llama")} onCancel={models.cancel}>
              {m.llama_present ? `Installed · ${m.backend === "cuda12" ? "CUDA 12" : m.backend === "vulkan" ? "Vulkan" : "CPU"}` : "Not downloaded"}
            </ModelLine>
            <ModelLine name="PDF reader" detail={`pdfium ${m.pdfium.build}`} optional
                       act={act("pdfium", m.pdfium.present)} label="Download" onGet={get("pdfium")} onCancel={models.cancel}>
              {m.pdfium.present ? "Installed" : "Downloads when a PDF is found"}
            </ModelLine>
            <ModelLine name="Document parser" detail={m.parser.model_id} optional
                       act={act("parser", m.parser.present)} label="Install" onGet={get("parser")} onCancel={models.cancel}>
              {!m.parser.present ? "Optional"
                : pLeft > 0 ? `${m.parser.server.running ? "Loaded" : "Installed"} · ${fmt(pTotal - pLeft)} parsed · ${fmt(pLeft)} left${pFailed ? ` · ${fmt(pFailed)} failed` : ""}`
                : pTotal > 0 ? `Done · ${fmt(pTotal)} pages read${pFailed ? ` · ${fmt(pFailed)} failed` : ""}`
                : m.parser.server.running ? "Loaded" : "Installed"}
            </ModelLine>
          </div>
        </section>
      )}
      {st?.error && <div className="fr-error" role="alert">Last error: {st.error}</div>}
    </>
  );
}

/** The Status page, embedded as a Settings section: names index, meaning index, models and files. */
function StatusSection({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [page, err, reload] = useStatusPage(backend);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [rebuilding, setRebuilding] = useState(false);

  const rescan = useCallback((letter: string) => {
    setBusy((b) => new Set(b).add(letter));
    backend.rescanVolume(letter).then(reload).catch(onError).finally(() => {
      setBusy((b) => { const n = new Set(b); n.delete(letter); return n; });
    });
  }, [backend, reload, onError]);
  const rebuild = useCallback(() => {
    setRebuilding(true);
    backend.tier1Rebuild().then(reload).catch(onError).finally(() => setRebuilding(false));
  }, [backend, reload, onError]);

  if (!page) {
    return err
      ? <div className="fr-error" role="alert">Could not read the status: {err}</div>
      : <Grp title="Status"><div className="grow stack"><Bar value={null} label="Loading" /></div></Grp>;
  }
  return (
    <>
      <Names t0={page.tier0} busy={busy} onRescan={rescan} />
      <StatusMeaning t1={page.tier1} onRebuild={rebuild} rebuilding={rebuilding}
                     onResume={() => { backend.tier1Pause(false).then(reload).catch(onError); }}
                     onRetryFfmpeg={() => { backend.ffmpegRetry().then(reload).catch(onError); }}
                     onReveal={(p) => { backend.revealPath(p).catch(onError); }} />
      <ModelsPane m={page.models} />
    </>
  );
}

/** Settings › AI agents: the one read-only switch, content scope, copy-ready connect lines and
 *  the activity log (PRD A6). */
function AgentsSection({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [cfg, setCfg] = useState<AgentConfig | null>(null);
  const [mcp, setMcp] = useState<McpPath | null>(null);
  const [log, setLog] = useState<AgentLogEntry[] | null>(null);
  const reload = useCallback(() => {
    backend.agentGetConfig().then(setCfg).catch(onError);
    backend.agentMcpPath().then(setMcp).catch(onError);
    backend.agentLog(50).then(setLog).catch(onError);
  }, [backend, onError]);
  // The log moves while an agent works; refresh while the section is on screen.
  useEffect(() => {
    reload();
    const t = setInterval(reload, 3000);
    return () => clearInterval(t);
  }, [reload]);
  const change = (patch: Partial<AgentConfig>) => {
    if (!cfg) return;
    const next = { ...cfg, ...patch };
    setCfg(next);
    backend.agentSetConfig(next).then(setCfg).catch((e) => { onError(e); reload(); });
  };
  const snip = mcp ? snippets(mcp.path) : null;
  const copy = (text: string) => backend.copyTextHtml(text).catch(onError);
  const rows: [string, string][] = snip
    ? [["Claude Code", snip.claudeCode], ["Claude Desktop / Cursor", snip.desktop], ["VS Code (.vscode/mcp.json)", snip.vscode], ["Codex (~/.codex/config.toml)", snip.codex]]
    : [];
  return (
    <>
      <Grp title="AI agents" label="Agent access">
        <Row title="Let AI agents search your files" desc="Read-only: agents can search and read, never open, change or delete anything. Every call is logged.">
          <Switch checked={!!cfg?.enabled} disabled={cfg === null} onChange={(v) => change({ enabled: v })} label="Let AI agents search with winseek" />
        </Row>
        <Row title="Let agents read file contents" desc="Applies to reading text, parsing documents and photo details. Name search is always on.">
          <Seg value={cfg?.content ?? null} disabled={!cfg?.enabled} onChange={(v) => change({ content: v })} label="What agents may read"
               options={[{ id: "indexed", label: "Indexed folders" }, { id: "any", label: "Any file" }]} />
        </Row>
      </Grp>

      <Grp title="Connect an agent">
        {mcp && !mcp.exists && (
          <div className="grow"><span className="gnote">winseek-mcp.exe is not installed next to the app yet (dev build). The snippets below still show where it will live.</span></div>
        )}
        {rows.map(([label, text]) => (
          <div className="grow stack" key={label}>
            <div className="idx-line">
              <b>{label}</b>
              <button className="gbtn" onClick={() => copy(text)}>Copy</button>
            </div>
            <code className="agent-snip" title={text}>{text}</code>
          </div>
        ))}
        {!snip && <div className="grow"><span className="gnote">Loading…</span></div>}
      </Grp>

      <Grp title="Activity">
        <Row title="Last 50 calls" desc="Who called what, and whether it worked. Paths are shown; file contents never are.">
          <button className="gbtn" onClick={() => backend.agentClearLog().then(reload).catch(onError)} disabled={!log || log.length === 0}>Clear log</button>
        </Row>
        <div className="grow stack">
          <ul className="agent-log" aria-label="Agent activity">
            {(log ?? []).slice().reverse().map((e, i) => (
              <li key={`${e.ts_ms}-${i}`} title={e.error ?? undefined}>
                <span className={"dot " + (e.ok ? "s-live" : "s-warn")} aria-hidden />
                <span className="nums muted">{formatDate(e.ts_ms)}</span>
                <span className="nums">{e.client}</span>
                <code>{e.tool}</code>
                <span className="muted agent-args">{argsPreview(e.args)}</span>
                {!e.ok && <span className="fr-error agent-err">{e.error}</span>}
              </li>
            ))}
            {log && log.length === 0 && <li className="none">No agent calls yet</li>}
          </ul>
        </div>
      </Grp>
    </>
  );
}

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

const HOTKEY_ROWS: { which: keyof Hotkeys; label: string; blurb: string; optional: boolean }[] = [
  { which: "popup", label: "Shortcut", blurb: "Opens winseek from anywhere. Combine Ctrl, Alt, Shift or Win with a key.", optional: false },
  { which: "library", label: "Open the Library", blurb: "A second shortcut, straight to your photos", optional: true },
  { which: "clipboard", label: "Search the clipboard", blurb: "Opens winseek with whatever you copied as the query", optional: true },
];

/** Settings › General › Shortcuts: the three global hotkeys, each with a press-to-record field
 *  (PRD B9). Recording never swallows keys: every press is captured, Esc cancels, Backspace
 *  clears the optional ones, Save applies. */
function ShortcutsBlock({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [hk, setHk] = useState<Hotkeys | null>(null);
  const [rec, setRec] = useState<{ which: keyof Hotkeys; combo: string | null } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let off: (() => void) | undefined, dead = false;
    backend.hotkeysGet().then(setHk).catch(onError);
    backend.onHotkeysChanged((h) => setHk(h)).then((u) => (dead ? u() : (off = u)));
    return () => { dead = true; off?.(); };
  }, [backend, onError]);
  const save = async () => {
    if (!rec) return;
    try {
      setHk(await backend.hotkeysSet(rec.which, rec.combo));
      setErr(null);
      setRec(null);
    } catch (e) {
      setErr(errText(e));
      setRec(null);
      backend.hotkeysGet().then(setHk).catch(onError);
    }
  };
  return (
    <Grp title="Shortcuts">
      {HOTKEY_ROWS.map((row) => {
        const bound = hk?.[row.which] ?? null;
        const recording = rec?.which === row.which;
        return (
          <Row key={row.which} title={row.label} desc={row.blurb}>
            {recording ? (
              <span className="hk-line">
                <span className="hk-rec" tabIndex={0} autoFocus role="textbox" aria-label="Press a shortcut"
                      onBlur={() => setRec(null)}
                      onKeyDown={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        if (e.key === "Escape") { setRec(null); return; }
                        if (e.key === "Backspace") { if (row.optional) setRec({ which: row.which, combo: null }); return; }
                        const c = eventToCombo(e);
                        if (c) setRec({ which: row.which, combo: c });
                      }}>
                  {rec.combo ?? "Press a shortcut… (Esc cancels)"}
                </span>
                <button className="gbtn pri" onMouseDown={(e) => e.preventDefault()} onClick={() => void save()}>Save</button>
                <button className="gbtn" onClick={() => setRec(null)}>Cancel</button>
              </span>
            ) : (
              <span className="hk-line">
                {bound ? <kbd className="hk-cur">{bound}</kbd> : <span className="val">None</span>}
                <button className="gbtn" onMouseDown={(e) => e.preventDefault()} onClick={() => { setErr(null); setRec({ which: row.which, combo: null }); }}>
                  {bound ? "Change" : "Set"}
                </button>
                {row.optional && bound && (
                  <button className="gbtn" onMouseDown={(e) => e.preventDefault()} onClick={() => { setRec({ which: row.which, combo: null }); void save(); }}>Remove</button>
                )}
              </span>
            )}
          </Row>
        );
      })}
      {err && <div className="grow"><span className="fr-error" role="alert">{err}</span></div>}
    </Grp>
  );
}

export type { SettingsSection };

/** General first, engine next, add-ons, diagnostics last (the known order); Ctrl+1..7 follow it. */
const NAV: { id: SettingsSection; label: string }[] = [
  { id: "general", label: "General" },
  { id: "indexing", label: "Indexing" },
  { id: "meaning", label: "Search by meaning" },
  { id: "launcher", label: "Apps and commands" },
  { id: "agents", label: "AI agents" },
  { id: "status", label: "Status" },
];

/** A hidden catalog id ("app:d:\\games\\x\\steam.exe", "setting:bluetooth") as a name plus where it is. */
function hiddenLabel(id: string): { name: string; where: string } {
  const cut = id.indexOf(":");
  const kind = cut > 0 ? id.slice(0, cut) : "";
  const rest = cut > 0 ? id.slice(cut + 1) : id;
  const slash = rest.lastIndexOf("\\");
  if (slash < 0) return { name: rest, where: kind === "setting" ? "Windows setting" : kind === "cmd" || kind === "command" ? "Command" : "App" };
  return { name: rest.slice(slash + 1).replace(/\.(exe|lnk)$/i, ""), where: rest.slice(0, slash) };
}

/** Launcher (apps / settings / commands) switches, extra program folders, hidden items, counts. */
function LauncherSection({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [cfg, setCfg] = useState<LaunchCfg | null>(null);
  const [counts, setCounts] = useState<LaunchCounts | null>(null);
  const [scanning, setScanning] = useState(false);
  const [folderText, setFolderText] = useState("");
  const reload = useCallback(() => {
    backend.launchGetConfig().then(setCfg).catch(onError);
    backend.launchCounts().then(setCounts).catch(onError);
  }, [backend, onError]);
  useEffect(() => {
    let off: (() => void) | undefined;
    reload();
    backend.onLaunchCatalogChanged(() => {
      setScanning(false);
      reload();
    }).then((u) => (off = u));
    return () => off?.();
  }, [backend, reload]);
  if (!cfg) return <Grp title="Apps and commands"><div className="grow"><span className="gnote">Loading…</span></div></Grp>;
  const save = (patch: Partial<LaunchCfg>) => {
    const next = { ...cfg, ...patch };
    setCfg(next);
    backend.launchSetConfig(next).then(setCfg).catch((e) => { onError(e); reload(); });
  };
  const addFolder = () => {
    const p = folderText.trim().replace(/^"(.*)"$/, "$1").replace(/[\\/]+$/, "");
    if (!p || cfg.folders.some((x) => lc(x) === lc(p))) return;
    setFolderText("");
    save({ folders: [...cfg.folders, p] });
  };
  const toggles: [keyof LaunchCfg, string, string][] = [
    ["apps", "Installed apps", "Start menu programs and Store apps, found by name, acronym or exe name"],
    ["settings", "Windows settings", "Settings pages and Control Panel items"],
    ["commands", "System commands", "Lock, sleep, restart, shut down, sign out, empty Recycle Bin"],
    ["portable", "Portable apps", "Programs that were never installed, found through the file index"],
    ["path", "Programs on your PATH", "Also list executables from the PATH folders. Off keeps the list short."],
    ["hide_uninstallers", "Hide uninstallers", "Installer and uninstaller entries never show up as apps"],
  ];
  return (
    <>
      <Grp title="Apps and commands" label="What the search box finds">
        {toggles.map(([key, title, hint]) => (
          <Row key={key} title={title} desc={hint}>
            <Switch checked={!!cfg[key]} onChange={(v) => save({ [key]: v })} label={title} />
          </Row>
        ))}
      </Grp>

      <Grp title="Extra program folders" note="Programs here are scanned up to four levels deep. Installing or removing apps re-scans automatically.">
        {cfg.folders.length > 0 && (
          <div className="glist">
            {cfg.folders.map((p) => (
              <div className="grow prow" key={p}>
                <span className="fpath" title={p}>{p}</span>
                <button className="fl-rm" onClick={() => save({ folders: cfg.folders.filter((x) => x !== p) })} aria-label={`Remove ${p}`}><XIcon /></button>
              </div>
            ))}
          </div>
        )}
        {cfg.folders.length === 0 && <div className="grow none">No extra folders</div>}
        <div className="grow addrow">
          <input className="gin" value={folderText} placeholder={"C:" + String.fromCharCode(92) + "tools"} spellCheck={false} aria-label="Folder to scan for programs"
                 onChange={(e) => setFolderText(e.target.value)}
                 onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addFolder(); } }} />
          <button className="gbtn" onClick={addFolder}>Add</button>
        </div>
      </Grp>

      <Grp title="Catalog">
        <Row title={counts ? `${fmt(counts.apps)} apps · ${fmt(counts.settings)} settings · ${fmt(counts.commands)} commands` : "Counting…"}
             desc="Refreshes on its own when the Start menu changes">
          <button className="gbtn" disabled={scanning} onClick={() => { setScanning(true); backend.launchReindex(); }}>
            {scanning ? "Scanning…" : "Re-scan apps"}
          </button>
        </Row>
        {cfg.hidden.map((id) => {
          const h = hiddenLabel(id);
          return (
            <Row key={id} title={<>{h.name} <em>hidden from results</em></>} desc={h.where} hint={id}>
              <button className="gbtn" onClick={() => save({ hidden: cfg.hidden.filter((x) => x !== id) })}>Unhide</button>
            </Row>
          );
        })}
        <Row title="Usage history" desc="Results you pick get a boost next time. Clearing forgets that, not the catalog.">
          <button className="gbtn" onClick={() => backend.launchClearHistory().catch(onError)}>Clear</button>
        </Row>
      </Grp>
    </>
  );
}

export function Settings({ backend, t1, models, onFolders, onRemoveFolder, onPause, onSetup, onClose, onHide: _onHide, onLibrary, section, onSection, onError, embedded, chrome, escHint = "back to search" }: {
  backend: Backend; t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onRemoveFolder: (path: string) => void; onPause: (p: boolean) => void;
  /** onClose: back to search. onHide: hide the whole popup. */
  onSetup: () => void; onClose: () => void; onHide: () => void; onLibrary: () => void;
  section: SettingsSection; onSection: (s: SettingsSection) => void; onError: (e: unknown) => void;
  /** Rendered in the full window beside the sidebar: no back button. */
  embedded?: boolean;
  /** Window controls, rendered at the right end of the top bar. */
  chrome?: ReactNode;
  /** What Esc does here (the footer hint): the search page, or back from Settings in the library window. */
  escHint?: string;
}) {
  const navRef = useRef<HTMLElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const [find, setFind] = useState("");
  const navItems = useMemo(() => filterSections(NAV, find), [find]);
  useEffect(() => {
    // Landing on a section puts the focus on its list entry so the arrow keys work at once; typing in
    // the "find" box keeps its focus.
    if (document.activeElement === findRef.current) return;
    (navRef.current?.querySelector<HTMLElement>(".nv.on")
      ?? bodyRef.current?.querySelector<HTMLElement>("input, button"))?.focus();
  }, [section]);
  // Arrow keys walk the sidebar; the popup's global key handler skips everything inside panels.
  const navKey = (e: React.KeyboardEvent) => {
    const fwd = e.key === "ArrowDown" || e.key === "ArrowRight", back = e.key === "ArrowUp" || e.key === "ArrowLeft";
    if (!fwd && !back) return;
    e.preventDefault();
    const items = Array.from(navRef.current?.querySelectorAll<HTMLElement>(".nv") ?? []);
    const at = items.indexOf(document.activeElement as HTMLElement);
    const next = items[(at + (fwd ? 1 : items.length - 1)) % items.length];
    next?.focus();
  };
  // Ctrl+1..7 jump to a section (PRD B8); the section heading takes the focus. Ctrl+F finds a setting.
  const sectionKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey) {
      if (e.key.toLowerCase() === "f") { e.preventDefault(); e.stopPropagation(); findRef.current?.focus(); findRef.current?.select(); return; }
      const at = NAV.findIndex((_, i) => matches(e, `Ctrl+${i + 1}`));
      if (at >= 0) {
        e.preventDefault();
        e.stopPropagation();
        onSection(NAV[at].id);
        requestAnimationFrame(() => {
          const h = bodyRef.current?.querySelector<HTMLElement>(".grp h3");
          if (h) { h.setAttribute("tabindex", "-1"); h.focus({ preventScroll: true }); }
        });
        return;
      }
    }
    trapKey(e);
  };
  return (
    <section className={"firstrun settings" + (embedded ? " embedded" : "")} aria-label="Settings"
             onKeyDown={(e) => {
               if (e.key === "Escape") {
                 e.preventDefault(); e.stopPropagation();
                 if (find) { setFind(""); return; } // the first Esc clears a search in progress
                 onClose();
               } else sectionKey(e);
             }}>
      <header className="tbar">
        {!embedded && (
          <button className="ibtn" onClick={onClose} title="Back to search (Esc)" aria-label="Back to search"><IChevL size={17} /></button>
        )}
        <h1>Settings</h1>
        <label className="sfind">
          <ISearch size={16} />
          <input ref={findRef} className="sf" value={find} placeholder="Find a setting" aria-label="Find a setting" spellCheck={false} autoComplete="off"
                 onChange={(e) => setFind(e.target.value)}
                 onKeyDown={(e) => {
                   if (e.key === "Enter" && navItems[0]) { e.preventDefault(); onSection(navItems[0].id); setFind(""); }
                   else if (e.key === "ArrowDown") { e.preventDefault(); navRef.current?.querySelector<HTMLElement>(".nv")?.focus(); }
                 }} />
        </label>
        {chrome}
      </header>
      <div className="set">
        <nav ref={navRef} className="snav" aria-label="Settings sections" onKeyDown={navKey}>
          {navItems.map((s) => (
            <button key={s.id} role="tab" aria-selected={section === s.id}
                    className={"nv" + (section === s.id ? " on" : "")} onClick={() => onSection(s.id)}>
              {s.label}
            </button>
          ))}
          {navItems.length === 0 && <span className="muted set-nomatch">No section matches</span>}
        </nav>
        <div className="scont" ref={bodyRef}>
          {/* the inner block has its own height, so the full window's card columns balance and scroll down, not sideways */}
          <div className="scols">
          {section === "meaning" && <Meaning backend={backend} t1={t1} models={models} onPause={onPause} onSetup={onSetup} />}
          {section === "indexing" && (
            <>
              <Folders t1={t1} onChange={onFolders} onRemove={onRemoveFolder} estimate={backend.tier1Estimate} />
              <CodeToggle backend={backend} onError={onError} />
              <IgnoredNames backend={backend} onError={onError} />
            </>
          )}
          {section === "launcher" && <LauncherSection backend={backend} onError={onError} />}
          {section === "agents" && <AgentsSection backend={backend} onError={onError} />}
          {section === "general" && (
            <>
              <Grp title="General">
                <AccentRow />
                <DefaultViewRow backend={backend} onError={onError} />
                <AutostartRow backend={backend} onError={onError} />
                <UpdatesRow backend={backend} onError={onError} />
                <VersionRow backend={backend} onError={onError} />
              </Grp>
              <Performance backend={backend} onError={onError} />
              <ShortcutsBlock backend={backend} onError={onError} />
              <Grp title="Library">
                <Row title="Photos and clean up" desc="Photos by month, collections, and finding near-duplicates or oversized files">
                  <button className="gbtn" onClick={onLibrary}>Open</button>
                </Row>
              </Grp>
            </>
          )}
          {section === "status" && <StatusSection backend={backend} onError={onError} />}
          </div>
        </div>
        <footer className="set-foot">
          <span>Changes are saved right away.</span>
          <span><kbd>Esc</kbd> {escHint} · <kbd>Ctrl</kbd> <kbd>F</kbd> find a setting</span>
        </footer>
      </div>
    </section>
  );
}
