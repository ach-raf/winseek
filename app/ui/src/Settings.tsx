import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, IgnoreList, ModelGroup, Profile, StatusModels, Tier1Status } from "./types";
import type { Models } from "./hooks";
import { rowAction, type RowAction } from "./modelRows";
import { Bar, Models as ModelsPane, Names, StatusMeaning, useStatusPage } from "./Status";
import { formatDuration, formatRate, formatSize } from "./format";
import { Folders, pausedText } from "./Setup";

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
                : m.parser.server.running ? `Loaded · ${fmt(m.parser.queue.queued)} queued`
                : `Installed · ${fmt(m.parser.queue.queued)} queued`}
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

export type SettingsSection = "meaning" | "indexing" | "performance" | "general" | "status";

const I = {
  meaning: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" aria-hidden><path d="M12 3.5c.65 4.1 2.75 6.2 6.9 6.9-4.15.7-6.25 2.8-6.9 6.9-.65-4.1-2.75-6.2-6.9-6.9 4.15-.7 6.25-2.8 6.9-6.9z" /><path d="M18.5 15.5c.3 1.9 1.3 2.9 3.2 3.2-1.9.3-2.9 1.3-3.2 3.2-.3-1.9-1.3-2.9-3.2-3.2 1.9-.3 2.9-1.3 3.2-3.2z" transform="scale(0.72) translate(7.2 6.6)" /></svg>,
  indexing: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /></svg>,
  performance: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M4.5 18a8.5 8.5 0 1 1 15 0" /><path d="m12 14 3.6-4.6" /><circle cx="12" cy="14.2" r="1.3" fill="currentColor" stroke="none" /></svg>,
  general: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden><path d="M4 8h8M17.5 8H20M4 16h3M12.5 16H20" /><circle cx="15" cy="8" r="2.2" /><circle cx="10" cy="16" r="2.2" /></svg>,
  status: <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M3 12h4l2.5-6.5 4 13L16.5 12H21" /></svg>,
};

const NAV: { id: SettingsSection; label: string; icon: React.ReactNode }[] = [
  { id: "meaning", label: "Search by meaning", icon: I.meaning },
  { id: "indexing", label: "Indexing", icon: I.indexing },
  { id: "performance", label: "Performance", icon: I.performance },
  { id: "general", label: "General", icon: I.general },
  { id: "status", label: "Status", icon: I.status },
];

export function Settings({ backend, t1, models, onFolders, onPause, onSetup, onClose, onLibrary, section, onSection, onError }: {
  backend: Backend; t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onPause: (p: boolean) => void;
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
        <div className="set-brand">winseek</div>
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
              <Folders t1={t1} onChange={onFolders} />
              <CodeToggle backend={backend} onError={onError} />
              <IgnoredNames backend={backend} onError={onError} />
            </>
          )}
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
