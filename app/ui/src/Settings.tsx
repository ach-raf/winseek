import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Backend, IgnoreList, Tier1Status } from "./types";
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

export function Settings({ backend, t1, onFolders, onPause, onSetup, onStatus, onClose, onError }: {
  backend: Backend; t1: Tier1Status | null;
  onFolders: (include: string[]) => void; onPause: (p: boolean) => void;
  onSetup: () => void; onStatus: () => void; onClose: () => void; onError: (e: unknown) => void;
}) {
  const root = useRef<HTMLElement>(null);
  useEffect(() => { root.current?.querySelector<HTMLElement>("input, button")?.focus(); }, []);
  const off = !t1 || t1.state === "models_missing" || t1.state === "unavailable";
  const s = t1?.stats;
  return (
    <section ref={root} className="firstrun setup settings" aria-label="Settings"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } }}>
      <h2>Settings</h2>

      <div className="set-row">
        <div>
          <div className="fl-title">Search by meaning</div>
          <div className="set-state">
            {!t1 ? "Loading…"
              : t1.state === "unavailable" ? `Unavailable: ${t1.error ?? "unknown error"}`
              : t1.state === "models_missing" ? "Not set up. The model files are not downloaded yet."
              : t1.paused_reason ? pausedText(t1.paused_reason)
              : s ? `${fmt(s.done)} of ${fmt(s.done + s.pending)} files indexed`
              : "Running"}
          </div>
        </div>
        {off ? (
          t1?.state === "models_missing" && <button className="btn primary" onClick={onSetup}>Set up…</button>
        ) : (
          (t1.paused_reason == null || t1.paused_reason === "user") && (
            <button className="btn quiet" onClick={() => onPause(t1.paused_reason == null)}>{t1.paused_reason == null ? "Pause indexing" : "Resume indexing"}</button>
          )
        )}
      </div>

      <Folders t1={t1} onChange={onFolders} />
      <CodeToggle backend={backend} onError={onError} />
      <IgnoredNames backend={backend} onError={onError} />

      <div className="actions">
        <span className="muted">Changes are saved right away.</span>
        <button className="btn quiet" onClick={onStatus}>Status…</button>
        <button className="btn primary" onClick={onClose}>Done</button>
      </div>
    </section>
  );
}
