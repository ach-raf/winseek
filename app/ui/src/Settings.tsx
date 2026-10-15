import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, IgnoreList, ModelGroup, Profile, StatusModels, Tier1Status } from "./types";
import type { Models } from "./hooks";
import { rowAction, type RowAction } from "./modelRows";
import { Bar, useStatusPage } from "./Status";
import { formatDuration, formatRate, formatSize } from "./format";
import { Folders, pausedText } from "./Setup";

const fmt = (n: number) => n.toLocaleString("en-US");
const lc = (s: string) => s.toLowerCase();

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
      <div className="fl-title">Ignored names</div>
      <p className="muted">Skipped anywhere inside the folders above. <code>*</code> and <code>?</code> are wildcards. Name search still finds them.</p>
      <div className="fl-add">
        <input value={text} onChange={(e) => { setText(e.target.value); setBad(null); }} placeholder="Add names, e.g. Raw*, scratch" aria-label="Add ignored names"
               spellCheck={false} autoComplete="off"
               onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); add(); } }} />
        <button className="btn quiet" onClick={add} disabled={!text.trim() || !ig}>Add</button>
      </div>
      {bad && <div className="fr-error" role="alert">{bad}</div>}
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
    <div className="set-row">
      <div>
        <div className="fl-title">Source code</div>
        <label className="set-state">
          <input type="checkbox" checked={!!on} disabled={on === null} onChange={(e) => change(e.target.checked)} aria-label="Index source code" />
          {" "}Index source code
        </label>
        <p className="muted">Searches the text of code files in your folders by content. Files ignored by a <code>.gitignore</code> are left out. Turning it off removes them from the index.</p>
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
    <div className="set-row">
      <div>
        <div className="fl-title">Startup</div>
        <label className="set-state">
          <input type="checkbox" checked={!!on} disabled={on === null} onChange={(e) => change(e.target.checked)} aria-label="Start with Windows" />
          {" "}Start with Windows
        </label>
        <p className="muted">Starts winseek in the tray when you sign in, so <kbd>Alt</kbd>+<kbd>Space</kbd> works right away. The name index runs as a service either way.</p>
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
    <div className="set-row perf">
      <div>
        <div className="fl-title">Performance</div>
        <select value={shown} disabled={p === null} onChange={(e) => change(e.target.value as Profile)} aria-label="Performance">
          {PROFILES.map((x) => <option key={x.id} value={x.id}>{x.label}</option>)}
        </select>
        <ul className="perf-list">
          {PROFILES.map((x) => (
            <li key={x.id} className={x.id === shown ? "on" : ""}><b>{x.label}</b> {"–"} {x.blurb}</li>
          ))}
        </ul>
      </div>
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
  const head = !t1 ? "Loading…"
    : t1.state === "unavailable" ? `Unavailable: ${t1.error ?? "unknown error"}`
    : t1.state === "models_missing" ? "Not set up. The model files are not downloaded yet."
    : pr ? pausedText(pr)
    : st?.state === "indexing" || t1.state === "indexing" ? "Indexing"
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
      <div className="set-row">
        <div className="fl-title">Search by meaning</div>
        {off ? (
          t1?.state === "models_missing" && <button className="btn primary" onClick={onSetup}>Set up…</button>
        ) : (
          (pr == null || pr === "user") && (
            <button className="btn quiet" onClick={() => onPause(pr == null)}>{pr == null ? "Pause indexing" : "Resume indexing"}</button>
          )
        )}
      </div>
      <div className={"set-state st-head " + (live ? "s-live" : "s-warn")}><span className="dot" />{head}</div>
      {s && total > 0 && (
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
      )}
      {m && (
        <ul className="models" aria-label="Models">
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
              : m.parser.server.running ? `Loaded · ${fmt(m.parser.queue.queued)} queued`
              : `Installed · ${fmt(m.parser.queue.queued)} queued`}
          </ModelLine>
        </ul>
      )}
      {st?.error && <div className="fr-error" role="alert">Last error: {st.error}</div>}
    </div>
  );
}

export function Settings({ backend, t1, models, onFolders, onPause, onSetup, onStatus, onClose, onError }: {
  backend: Backend; t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onPause: (p: boolean) => void;
  onSetup: () => void; onStatus: () => void; onClose: () => void; onError: (e: unknown) => void;
}) {
  const root = useRef<HTMLElement>(null);
  useEffect(() => { root.current?.querySelector<HTMLElement>("input, button")?.focus(); }, []);
  return (
    <section ref={root} className="firstrun setup settings" aria-label="Settings"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } }}>
      <h2>Settings</h2>

      <Meaning backend={backend} t1={t1} models={models} onPause={onPause} onSetup={onSetup} />

      <Folders t1={t1} onChange={onFolders} />
      <Performance backend={backend} onError={onError} />
      <CodeToggle backend={backend} onError={onError} />
      <IgnoredNames backend={backend} onError={onError} />
      <AutostartToggle backend={backend} onError={onError} />

      <div className="actions">
        <span className="muted">Changes are saved right away.</span>
        <button className="btn secondary" onClick={onStatus} title="Name index volumes, indexing details and model files">View status</button>
        <button className="btn primary" onClick={onClose}>Done</button>
      </div>
    </section>
  );
}
