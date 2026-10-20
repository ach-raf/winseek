import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentConfig, AgentLogEntry, Backend, IgnoreList, LaunchCfg, LaunchCounts, McpPath, ModelGroup, Profile, StatusModels, Tier1Status } from "./types";
import type { Models } from "./hooks";
import { rowAction, type RowAction } from "./modelRows";
import { argsPreview, snippets } from "./agentLogic";
import { Bar, Models as ModelsPane, Names, StatusMeaning, useStatusPage } from "./Status";
import { formatDate, formatDuration, formatPages, formatRate, formatSize } from "./format";
import { Folders, pausedText } from "./Setup";
import logo from "./assets/logo.png";

const fmt = (n: number) => n.toLocaleString("en-US");
const lc = (s: string) => s.toLowerCase();

const Switch = ({ checked, disabled, onChange, label }: {
  checked: boolean; disabled?: boolean; onChange: (v: boolean) => void; label: string;
}) => (
  <label className="switch">
    <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} aria-label={label} />
  </label>
);

const Check = () => (
  <svg className="set-check" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    <path d="m5 12.5 4.5 4.5L19 7.5" />
  </svg>
);

/** Split pasted text into names: commas, semicolons and new lines separate entries. */
function parseNames(text: string): string[] {
  return text.split(/[,;\r\n]+/).map((s) => s.trim().replace(/^"(.*)"$/, "$1").replace(/^[\\/]+|[\\/]+$/g, "")).filter(Boolean);
}

function IgnoredNames({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [ig, setIg] = useState<IgnoreList | null>(null);
  const [filter, setFilter] = useState("");
  const [text, setText] = useState("");
  const [bad, setBad] = useState<string | null>(null);
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
    <div className="ignored">
      <div className="fl-title">Skipped names</div>
      <p className="muted set-note">Skipped anywhere inside the folders above. <code>*</code> and <code>?</code> are wildcards. Name search still finds them.</p>
      <div className="set-group">
        <div className="ig-add">
          <input value={text} onChange={(e) => { setText(e.target.value); setBad(null); }} placeholder="Add names, e.g. Raw*, scratch" aria-label="Add ignored names"
                 spellCheck={false} autoComplete="off"
                 onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); add(); } }} />
          <button className="btn quiet" onClick={add} disabled={!text.trim() || !ig}>Add</button>
        </div>
        {bad && <div className="ig-err fr-error" role="alert">{bad}</div>}
        <div className="ig-bar">
          <input className="ig-filter" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter" aria-label="Filter ignored names"
                 spellCheck={false} autoComplete="off" />
          <span className="nums">
            {ig ? `${fmt(ig.list.length)} name${ig.list.length === 1 ? "" : "s"}` : "Loading…"}
            {ig && (isDefault ? " · defaults" : removedDefaults ? ` · ${fmt(removedDefaults)} default${removedDefaults === 1 ? "" : "s"} removed` : "")}
          </span>
          <button className="btn quiet" onClick={() => save(null)} disabled={!ig || isDefault}>Reset to defaults</button>
        </div>
        <ul className="ig-list" aria-label="Ignored names">
          {shown.map((n) => (
            <li key={n} className={defaults.has(lc(n)) ? "" : "custom"} title={defaults.has(lc(n)) ? n : `${n} (added by you)`}>
              <span>{n}</span>
              <button className="ig-x" onClick={() => remove(n)} aria-label={`Stop ignoring ${n}`} title="Remove">
                <svg viewBox="0 0 24 24" width="10" height="10" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
              </button>
            </li>
          ))}
          {ig && shown.length === 0 && <li className="none">{ig.list.length ? "No name matches the filter" : "Nothing is ignored"}</li>}
        </ul>
      </div>
    </div>
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
    <div className="set-group">
      <div className="set-row">
        <span className="set-optmain">
          <b>Index source code</b>
          <span className="muted">Searches the text of code files in your folders by content. Files ignored by a <code>.gitignore</code> are left out. Turning it off removes them from the index.</span>
        </span>
        <Switch checked={!!on} disabled={on === null} onChange={change} label="Index source code" />
      </div>
    </div>
  );
}

function AutostartToggle({ backend, onError }: { backend: Backend; onError: (e: unknown) => void }) {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => { backend.autostartGet().then(setOn).catch(onError); }, [backend, onError]);
  const change = (v: boolean) => {
    setOn(v);
    backend.autostartSet(v).then(setOn).catch((e) => { setOn(!v); onError(e); });
  };
  return (
    <div className="set-group">
      <div className="set-row">
        <span className="set-optmain">
          <b>Start with Windows</b>
          <span className="muted">Starts winseek in the tray when you sign in, so <kbd>Alt</kbd>+<kbd>Space</kbd> works right away. The name index runs as a service either way.</span>
        </span>
        <Switch checked={!!on} disabled={on === null} onChange={change} label="Start with Windows" />
      </div>
    </div>
  );
}

const PROFILES: { id: Profile; label: string; blurb: string }[] = [
  { id: "low", label: "Low", blurb: "Least graphics memory. Slow on images and documents." },
  { id: "balanced", label: "Balanced", blurb: "Search on the processor, image and document models load on the graphics card only while they work." },
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
    <div className="perf">
      <div className="set-group" role="radiogroup" aria-label="Performance profile">
        {PROFILES.map((x) => (
          <button key={x.id} role="radio" aria-checked={x.id === shown} disabled={p === null}
                  className={"set-row set-opt" + (x.id === shown ? " on" : "")} onClick={() => change(x.id)}>
            <span className="set-optmain"><b>{x.label}</b><span className="muted">{x.blurb}</span></span>
            <Check />
          </button>
        ))}
      </div>
      <p className="muted set-note">Applies to search by meaning: where the models run and how long they stay loaded. Name search is not affected.</p>
    </div>
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
  onGet: () => void; onCancel: () => void; children: React.ReactNode;
}) {
  const ok = act.kind === "installed" ? true : optional ? null : false;
  const size = (b: number | null) => (b != null ? ` · ${formatSize(b)}` : "");
  const paused = act.kind === "failed" && act.error === "cancelled";
  return (
    <li className={ok === true ? "ok" : ok === false ? "miss" : "opt"}>
      <span className="mi" aria-hidden>{ok === true ? "✓" : ok === false ? "!" : "–"}</span>
      <span className="mn"><b>{name}</b>{detail && <span className="muted"> · {detail}</span>}</span>
      <span className="ms">
        {act.kind === "installed" ? children
          : act.kind === "downloading" ? (
            <>
              <span className="nums">{act.total > 0 ? `${formatSize(act.got)} of ${formatSize(act.total)}` : "Starting…"}</span>
              <button className="btn quiet sm" onClick={onCancel}>Cancel</button>
            </>
          ) : act.kind === "waiting" ? <span className="muted">Waiting for the current download</span>
          : act.kind === "failed" ? (
            <button className="btn quiet sm" onClick={onGet}>{paused ? "Resume" : "Retry"}{size(act.bytes)}</button>
          ) : (
            <>
              <span className="muted">{children}</span>
              <button className="btn quiet sm" onClick={onGet}>{label}{size(act.bytes)}</button>
            </>
          )}
      </span>
      {act.kind === "downloading" && <div className="ml-sub"><Bar value={act.frac} label={`Downloading ${name}`} /></div>}
      {act.kind === "failed" && (
        <div className={"ml-sub " + (paused ? "muted" : "fr-error")} role={paused ? "status" : "alert"}>
          {paused ? "Download paused. Resume continues where it stopped." : `Download failed: ${act.error}. Retry continues where it stopped.`}
        </div>
      )}
    </li>
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
    : s && s.pending > 0 ? "Waiting for work" : "All caught up";
  const live = !off && !pr;
  const files = m?.files ?? [];
  const embedOk = files.length > 0 && files.every((f) => f.bytes != null && f.bytes === f.expected_bytes);
  const embedSize = files.reduce((a, f) => a + (f.bytes ?? 0), 0);
  const sv = m?.server;
  const act = (g: ModelGroup, installed: boolean) => rowAction(g, installed, models);
  const get = (g: ModelGroup) => () => models.start(g === "parser", [g]);
  return (
    <div className="meaning" aria-label="Search by meaning">
      <div className="set-group">
        <div className="set-row">
          <span className="set-optmain">
            <b>Indexing</b>
            <span className={"st-head " + (live ? "s-live" : "s-warn")}><span className="dot" />{head}</span>
          </span>
          {off ? (
            t1?.state === "models_missing" && <button className="btn primary" onClick={onSetup}>Set up…</button>
          ) : (
            (pr == null || pr === "user") && (
              <button className="btn quiet" onClick={() => onPause(pr == null)}>{pr == null ? "Pause" : "Resume"}</button>
            )
          )}
        </div>
        {s && total > 0 && (
          <div className="set-row col">
            <div className="st-cov">
              <div className="idx-line">
                <span className="nums">{fmt(s.done)} of {fmt(total)} files</span>
                <span className="nums muted">
                  {[st?.rate != null && st.rate > 0 ? formatRate(st.rate) : null,
                    st?.eta_s != null && live ? `about ${formatDuration(st.eta_s)} left` : null].filter(Boolean).join(" · ")}
                </span>
              </div>
              <Bar value={total ? s.done / total : null} label="Files indexed" />
            </div>
          </div>
        )}
        {m?.parser.present && pTotal > 0 && (pLeft > 0 || pRate != null) && (
          <div className="set-row col">
            <div className="st-cov">
              <div className="idx-line">
                <span className="nums">{fmt(pTotal - pLeft)} of {fmt(pTotal)} pages</span>
                <span className="nums muted">
                  {[pRate != null ? formatPages(pRate) : null,
                    pRate != null && pEta != null && pLeft > 0 && !pr ? `about ${formatDuration(pEta)} left` : null].filter(Boolean).join(" · ")}
                </span>
              </div>
              <Bar value={pTotal ? (pTotal - pLeft) / pTotal : null} label="Pages read by the document parser" />
            </div>
          </div>
        )}
      </div>
      {m && (
        <>
          <div className="fl-title">Components</div>
          <ul className="set-group models" aria-label="Models">
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
          </ul>
        </>
      )}
      {st?.error && <div className="fr-error" role="alert">Last error: {st.error}</div>}
    </div>
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
      : <div className="vols"><Bar value={null} label="Loading" /><span className="muted">Loading…</span></div>;
  }
  return (
    <div className="st-cols">
      <Names t0={page.tier0} busy={busy} onRescan={rescan} />
      <StatusMeaning t1={page.tier1} onRebuild={rebuild} rebuilding={rebuilding} />
      <ModelsPane m={page.models} />
    </div>
  );
}

/** Settings › Agents: the one read-only switch, content scope, copy-ready connect lines and
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
      <div className="set-sec">
        <div className="fl-title">Agent access</div>
        <div className="set-group">
          <div className="set-row">
            <span className="set-optmain">
              <b>Let AI agents search with winseek</b>
              <span className="muted">Read-only: agents can search and read, never open, change or delete anything. Every call is logged below.</span>
            </span>
            <Switch checked={!!cfg?.enabled} disabled={cfg === null} onChange={(v) => change({ enabled: v })} label="Let AI agents search with winseek" />
          </div>
          <div className="set-row" role="radiogroup" aria-label="What agents may read">
            <span className="set-optmain">
              <b>Let agents read file contents</b>
              <span className="muted">Applies to reading text, parsing documents and photo details. Name search is always on.</span>
            </span>
            <span className="set-seg" role="presentation">
              <button role="radio" aria-checked={cfg?.content === "indexed"} disabled={!cfg?.enabled}
                      className={"set-segbtn" + (cfg?.content === "indexed" ? " on" : "")} onClick={() => change({ content: "indexed" })}>
                Indexed folders only
              </button>
              <button role="radio" aria-checked={cfg?.content === "any"} disabled={!cfg?.enabled}
                      className={"set-segbtn" + (cfg?.content === "any" ? " on" : "")} onClick={() => change({ content: "any" })}>
                Any file I can open
              </button>
            </span>
          </div>
        </div>
      </div>

      <div className="set-sec">
        <div className="fl-title">Connect an agent</div>
        <div className="set-group">
          {mcp && !mcp.exists && (
            <div className="set-row"><span className="muted">winseek-mcp.exe is not installed next to the app yet (dev build). The snippets below still show where it will live.</span></div>
          )}
          {rows.map(([label, text]) => (
            <div className="set-row col" key={label}>
              <div className="idx-line">
                <b>{label}</b>
                <button className="btn quiet sm" onClick={() => copy(text)}>Copy</button>
              </div>
              <code className="agent-snip" title={text}>{text}</code>
            </div>
          ))}
          {!snip && <div className="set-row"><span className="muted">Loading…</span></div>}
        </div>
      </div>

      <div className="set-sec">
        <div className="fl-title">Activity</div>
        <div className="set-group">
          <div className="set-row">
            <span className="set-optmain">
              <b>Last 50 calls</b>
              <span className="muted">Who called what, and whether it worked. Paths are shown; file contents never are.</span>
            </span>
            <button className="btn secondary" onClick={() => backend.agentClearLog().then(reload).catch(onError)} disabled={!log || log.length === 0}>
              Clear log
            </button>
          </div>
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
      </div>
    </>
  );
}

export type SettingsSection = "meaning" | "indexing" | "launcher" | "agents" | "performance" | "general" | "status";

const I = {
  meaning: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" aria-hidden><path d="M12 3.5c.65 4.1 2.75 6.2 6.9 6.9-4.15.7-6.25 2.8-6.9 6.9-.65-4.1-2.75-6.2-6.9-6.9 4.15-.7 6.25-2.8 6.9-6.9z" /><path d="M18.5 15.5c.3 1.9 1.3 2.9 3.2 3.2-1.9.3-2.9 1.3-3.2 3.2-.3-1.9-1.3-2.9-3.2-3.2 1.9-.3 2.9-1.3 3.2-3.2z" transform="scale(0.72) translate(7.2 6.6)" /></svg>,
  indexing: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /></svg>,
  launcher: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><rect x="3.5" y="3.5" width="7" height="7" rx="1.6" /><rect x="13.5" y="3.5" width="7" height="7" rx="1.6" /><rect x="3.5" y="13.5" width="7" height="7" rx="1.6" /><path d="M17 13.5v7M13.5 17h7" /></svg>,
  agents: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h9M17 18h3" /><circle cx="15" cy="6" r="2" /><circle cx="9" cy="12" r="2" /><circle cx="15" cy="18" r="2" /></svg>,
  performance: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M4.5 18a8.5 8.5 0 1 1 15 0" /><path d="m12 14 3.6-4.6" /><circle cx="12" cy="14.2" r="1.3" fill="currentColor" stroke="none" /></svg>,
  general: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden><path d="M4 8h8M17.5 8H20M4 16h3M12.5 16H20" /><circle cx="15" cy="8" r="2.2" /><circle cx="10" cy="16" r="2.2" /></svg>,
  status: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M3 12h4l2.5-6.5 4 13L16.5 12H21" /></svg>,
};

const NAV: { id: SettingsSection; label: string; icon: React.ReactNode }[] = [
  { id: "meaning", label: "Search by meaning", icon: I.meaning },
  { id: "indexing", label: "Indexing", icon: I.indexing },
  { id: "launcher", label: "Launcher", icon: I.launcher },
  { id: "agents", label: "Agents", icon: I.agents },
  { id: "performance", label: "Performance", icon: I.performance },
  { id: "general", label: "General", icon: I.general },
  { id: "status", label: "Status", icon: I.status },
];

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
  if (!cfg) return <div className="vols"><span className="muted">Loading…</span></div>;
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
    ["apps", "Installed apps", "Start menu programs and Store apps, searchable by name, acronym or exe name."],
    ["settings", "Windows settings", "Settings pages and Control Panel items, opened straight from the box."],
    ["commands", "System commands", "Lock, sleep, restart, shut down, sign out, and the Recycle Bin."],
    ["portable", "Portable apps from the file index", "An .exe in your indexed folders whose name matches what you type joins the Apps section."],
    ["path", "Search PATH folders", "Also list executables from the directories on PATH. Off: it is a long, noisy list."],
    ["hide_uninstallers", "Hide uninstallers", "Installer and uninstaller entries never show up as apps."],
  ];
  return (
    <>
      <div className="set-sec">
        <div className="fl-title">What the search box finds</div>
        <div className="set-group">
          {toggles.map(([key, title, hint]) => (
            <div className="set-row" key={key}>
              <span className="set-optmain">
                <b>{title}</b>
                <span className="muted">{hint}</span>
              </span>
              <Switch checked={!!cfg[key]} onChange={(v) => save({ [key]: v })} label={title} />
            </div>
          ))}
        </div>
      </div>

      <div className="set-sec">
        <div className="fl-title">Extra program folders</div>
        <div className="set-group">
          <ul className="fl-list">
            {cfg.folders.map((p) => (
              <li key={p}>
                <label title={p}><span>{p}</span></label>
                <button className="fl-rm" onClick={() => save({ folders: cfg.folders.filter((x) => x !== p) })} aria-label={`Remove ${p}`}>
                  <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden><path d="M18 6 6 18M6 6l12 12" /></svg>
                </button>
              </li>
            ))}
            {cfg.folders.length === 0 && <li className="none">No extra folders</li>}
          </ul>
          <div className="fl-add">
            <input value={folderText} placeholder={"C:" + String.fromCharCode(92) + "tools"} spellCheck={false} aria-label="Folder to scan for programs"
                   onChange={(e) => setFolderText(e.target.value)}
                   onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addFolder(); } }} />
            <button className="btn secondary" onClick={addFolder}>Add</button>
          </div>
          <span className="muted set-note">Programs here are scanned up to four levels deep. Installing or removing apps re-scans automatically.</span>
        </div>
      </div>

      <div className="set-sec">
        <div className="fl-title">Catalog</div>
        <div className="set-group">
          <div className="set-row">
            <span className="set-optmain">
              <b>{counts ? `${fmt(counts.apps)} apps · ${fmt(counts.settings)} settings · ${fmt(counts.commands)} commands` : "Counting…"}</b>
              <span className="muted">The catalog lives in memory and refreshes on its own when the Start menu changes.</span>
            </span>
            <button className="btn secondary" disabled={scanning} onClick={() => { setScanning(true); backend.launchReindex(); }}>
              {scanning ? "Scanning…" : "Re-scan apps"}
            </button>
          </div>
          {cfg.hidden.length > 0 && (
            <>
              <div className="fl-title" style={{ marginTop: 10 }}>Hidden items · {cfg.hidden.length}</div>
              <ul className="fl-list">
                {cfg.hidden.map((id) => (
                  <li key={id}>
                    <label title={id}><span>{id}</span></label>
                    <button className="btn secondary" onClick={() => save({ hidden: cfg.hidden.filter((x) => x !== id) })}>Unhide</button>
                  </li>
                ))}
              </ul>
            </>
          )}
          <div className="set-row">
            <span className="set-optmain">
              <b>Usage history</b>
              <span className="muted">Results you pick get a boost next time. Clearing forgets that, not the catalog.</span>
            </span>
            <button className="btn secondary" onClick={() => backend.launchClearHistory().catch(onError)}>Clear usage history</button>
          </div>
        </div>
      </div>
    </>
  );
}

export function Settings({ backend, t1, models, onFolders, onRemoveFolder, onPause, onSetup, onClose, onLibrary, section, onSection, onError }: {
  backend: Backend; t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onRemoveFolder: (path: string) => void; onPause: (p: boolean) => void;
  onSetup: () => void; onClose: () => void; onLibrary: () => void;
  section: SettingsSection; onSection: (s: SettingsSection) => void; onError: (e: unknown) => void;
}) {
  const navRef = useRef<HTMLElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    (navRef.current?.querySelector<HTMLElement>(".set-navitem.on")
      ?? bodyRef.current?.querySelector<HTMLElement>("input, button"))?.focus();
  }, [section]);
  // Arrow keys walk the sidebar; the popup's global key handler skips everything inside panels.
  const navKey = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = Array.from(navRef.current?.querySelectorAll<HTMLElement>(".set-navitem") ?? []);
    const at = items.indexOf(document.activeElement as HTMLElement);
    const next = items[(at + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length];
    next?.focus();
  };
  return (
    <section className="firstrun setup settings" aria-label="Settings"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } }}>
      <div className="set-side">
        <div className="set-brand"><img src={logo} alt="" width={22} height={22} />winseek</div>
        <nav ref={navRef} className="set-nav" aria-label="Settings sections" onKeyDown={navKey}>
          {NAV.map((s) => (
            <button key={s.id} role="tab" aria-selected={section === s.id}
                    className={"set-navitem" + (section === s.id ? " on" : "")} onClick={() => onSection(s.id)}>
              {s.icon}<span>{s.label}</span>
            </button>
          ))}
        </nav>
      </div>
      <div className="set-detail">
        <div className="set-body" ref={bodyRef}>
          {section === "meaning" && <Meaning backend={backend} t1={t1} models={models} onPause={onPause} onSetup={onSetup} />}
          {section === "indexing" && (
            <>
              <Folders t1={t1} onChange={onFolders} onRemove={onRemoveFolder} />
              <CodeToggle backend={backend} onError={onError} />
              <IgnoredNames backend={backend} onError={onError} />
            </>
          )}
          {section === "launcher" && <LauncherSection backend={backend} onError={onError} />}
          {section === "agents" && <AgentsSection backend={backend} onError={onError} />}
          {section === "performance" && <Performance backend={backend} onError={onError} />}
          {section === "general" && (
            <>
              <AutostartToggle backend={backend} onError={onError} />
              <div className="set-sec">
                <div className="fl-title">Library</div>
                <div className="set-group">
                  <div className="set-row">
                    <span className="set-optmain">
                      <b>Photos &amp; clean up</b>
                      <span className="muted">Photos by month, smart collections, and finding near-duplicates or oversized files.</span>
                    </span>
                    <button className="btn secondary" onClick={onLibrary}>Open</button>
                  </div>
                </div>
              </div>
            </>
          )}
          {section === "status" && <StatusSection backend={backend} onError={onError} />}
        </div>
        <footer className="set-foot">
          <span className="muted">Changes are saved right away.</span>
          <button className="btn primary" onClick={onClose}>Done</button>
        </footer>
      </div>
    </section>
  );
}
