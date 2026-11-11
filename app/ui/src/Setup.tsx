import { useEffect, useRef, useState } from "react";
import type { Models } from "./hooks";
import type { ModelsPlan, PausedReason, Tier1Status } from "./types";
import { formatSize } from "./format";
import { trapKey } from "./trap";
import { Grp, Row, XIcon } from "./settingsUi";

const fmt = (n: number) => n.toLocaleString("en-US");

export function backendLine(plan: ModelsPlan): string {
  const how = plan.backend === "cuda12" ? "using CUDA" : plan.backend === "vulkan" ? "using Vulkan" : "using the CPU";
  return plan.gpu ? `${plan.gpu}, ${how}` : `No dedicated GPU found, ${how}`;
}

export function pausedText(r: PausedReason): string {
  return r === "user" ? "Paused by you"
    : r === "battery" ? "Paused on battery power. It resumes when you plug in."
    : "Paused while a full-screen game or presentation runs. It resumes afterwards.";
}

function Bar({ value, label }: { value: number | null; label: string }) {
  const pct = value == null ? 0 : Math.max(0, Math.min(100, value * 100));
  return (
    <div className={"bar" + (value == null ? " indet" : "")} role="progressbar" aria-label={label}
         aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
      <i style={{ width: pct + "%" }} />
    </div>
  );
}

export function Folders({ t1, onChange, onRemove }: { t1: Tier1Status | null; onChange: (include: string[]) => void; onRemove: (path: string) => void }) {
  const [known, setKnown] = useState<string[]>([]); // every folder seen this session, so an unchecked one can be re-checked
  const [text, setText] = useState("");
  const include = t1?.include ?? [];
  useEffect(() => {
    setKnown((k) => {
      const add = include.filter((p) => !k.some((x) => x.toLowerCase() === p.toLowerCase()));
      return add.length ? [...k, ...add] : k;
    });
  }, [include.join("|")]); // eslint-disable-line react-hooks/exhaustive-deps
  const on = (p: string) => include.some((x) => x.toLowerCase() === p.toLowerCase());
  const toggle = (p: string) => onChange(on(p) ? include.filter((x) => x.toLowerCase() !== p.toLowerCase()) : [...include, p]);
  const add = () => {
    const p = text.trim().replace(/^"(.*)"$/, "$1").replace(/[\\/]+$/, "");
    if (!p) return;
    setText("");
    if (!on(p)) onChange([...include, p]);
  };
  // Removal is final (the backend forgets the folder's indexed files), unlike unchecking,
  // which keeps both the entry and the data for a quick re-check.
  const remove = (p: string) => {
    setKnown((k) => k.filter((x) => x.toLowerCase() !== p.toLowerCase()));
    onRemove(p);
  };
  return (
    <Grp title={`Folders to index${known.length > 0 ? ` · ${known.length}` : ""}`}>
      {known.length > 0 && (
        <div className="glist">
          {known.map((p) => (
            <div className="grow frow" key={p}>
              <label className="fpath" title={p}>
                <input type="checkbox" checked={on(p)} onChange={() => toggle(p)} />
                <span>{p}</span>
              </label>
              <button className="fl-rm" onClick={() => remove(p)} aria-label={`Remove ${p}`}
                      title="Remove: stops indexing this folder and forgets the files already indexed">
                <XIcon />
              </button>
            </div>
          ))}
        </div>
      )}
      {known.length === 0 && <div className="grow none">No folders yet</div>}
      <div className="grow addrow">
        <input className="gin" value={text} onChange={(e) => setText(e.target.value)} placeholder="Add a folder, e.g. D:\Photos" aria-label="Add a folder"
               spellCheck={false} autoComplete="off"
               onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); add(); } }} />
        <button className="gbtn" onClick={add} disabled={!text.trim()}>Add</button>
      </div>
    </Grp>
  );
}

function Indexing({ t1, onPause }: { t1: Tier1Status | null; onPause: (p: boolean) => void }) {
  if (!t1) return <div className="vols"><Bar value={null} label="Loading" /></div>;
  if (t1.state === "unavailable") return <div className="fr-error" role="alert">Search by meaning is unavailable: {t1.error ?? "unknown error"}</div>;
  const s = t1.stats;
  const total = s ? s.done + s.pending : 0;
  const frac = s && total > 0 ? s.done / total : null;
  const starting = t1.state === "models_missing";
  return (
    <Grp title="Indexing">
      <div className="grow stack">
        <div className="idx-line">
          <b>{starting ? "Starting…" : t1.state === "idle" && total > 0 && s!.pending === 0 ? "All caught up" : t1.state === "paused" ? "Indexing paused" : "Indexing in the background"}</b>
          <span className="nums">{s ? `${fmt(s.done)} of ${fmt(total)} files` : ""}</span>
        </div>
        <Bar value={starting ? null : (frac ?? 1)} label="Indexing progress" />
        {t1.paused_reason && <div className="idx-note" role="status">{pausedText(t1.paused_reason)}</div>}
        {s && s.failed > 0 && <div className="idx-note">{fmt(s.failed)} file{s.failed === 1 ? "" : "s"} could not be read</div>}
        {t1.error && <div className="fr-error" role="alert">{t1.error}</div>}
      </div>
      {(t1.state === "indexing" || t1.state === "paused") && (t1.paused_reason == null || t1.paused_reason === "user") && (
        <Row title={t1.paused_reason == null ? "Pause indexing" : "Resume indexing"}>
          <button className="gbtn" onClick={() => onPause(t1.paused_reason == null)}>{t1.paused_reason == null ? "Pause" : "Resume"}</button>
        </Row>
      )}
    </Grp>
  );
}

const PARSER_DESC = "Reads scanned pages, receipts and screenshots into text and tables you can search and copy";

export function Setup({ t1, models, onFolders, onRemoveFolder, onPause, onClose, primaryRef }: {
  t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onRemoveFolder: (path: string) => void; onPause: (p: boolean) => void; onClose: () => void;
  primaryRef: React.MutableRefObject<() => void>;
}) {
  const { plan, running, progress, interrupted } = models;
  // The optional document parser never blocks the default setup: it is a switch (and, once the rest is
  // installed, a separate offer).
  const [includeParser, setIncludeParser] = useState(false);
  const optional = plan ? plan.items.filter((i) => i.optional) : [];
  const optMissing = optional.filter((i) => !i.present);
  const parserBytes = optMissing.reduce((a, i) => a + (i.bytes ?? 0), 0);
  const missing = plan ? plan.items.some((i) => !i.present && !i.optional) : false;
  const phase = running ? "downloading" : !plan ? "loading" : missing ? (interrupted ? "interrupted" : "choose") : "indexing";
  primaryRef.current = () => { if (phase === "choose" || phase === "interrupted") models.start(includeParser); else if (phase === "indexing") onClose(); };
  const root = useRef<HTMLElement>(null);
  const overall = progress && progress.overall_total > 0 ? progress.overall_downloaded / progress.overall_total : null;
  const cancelled = interrupted === "cancelled";

  return (
    <section ref={root} className="firstrun setup" aria-label="Search by meaning setup"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } else trapKey(e); }}>
      <h2>Want search by meaning?</h2>
      <p>Search &ldquo;sunset at the beach&rdquo; and get beach photos, whatever they&rsquo;re named. Everything stays on this PC. Name search keeps working either way.</p>

      <div className={phase === "choose" || phase === "interrupted" || phase === "loading" ? "setup-cols" : undefined}>
      {(phase === "choose" || phase === "interrupted" || phase === "loading") && <Folders t1={t1} onChange={onFolders} onRemove={onRemoveFolder} />}

      {phase === "loading" && <div className="vols"><Bar value={null} label="Detecting your GPU" /><span className="muted">Detecting your GPU…</span></div>}

      {plan && (phase === "choose" || phase === "interrupted" || phase === "downloading") && (
        <Grp title={backendLine(plan)} note={phase !== "downloading" ? (
          <>Download {formatSize(plan.total_bytes_missing + (includeParser ? parserBytes : 0))}{plan.size_unknown ? " plus the llama.cpp runtime" : ""}</>
        ) : undefined}>
          {plan.items.filter((i) => !i.optional).map((i) => (
            <Row key={i.name} title={i.name}>
              <span className={"val" + (i.present ? " ok" : "")}>{i.present ? "Installed" : i.bytes != null ? formatSize(i.bytes) : "size shown when it starts"}</span>
            </Row>
          ))}
          {optional.length > 0 && (
            <Row title="Document parser" desc={`Optional. ${PARSER_DESC}`}>
              <span className={"val" + (optMissing.length === 0 ? " ok" : "")}>{optMissing.length === 0 ? "Installed" : formatSize(parserBytes)}</span>
              {optMissing.length > 0 && phase !== "downloading" && (
                <label className="switch">
                  <input type="checkbox" checked={includeParser} onChange={(e) => setIncludeParser(e.target.checked)} aria-label="Also download the document parser" />
                </label>
              )}
            </Row>
          )}
        </Grp>
      )}
      </div>

      {phase === "downloading" && (
        <Grp title="Downloading">
          <div className="grow stack">
            <div className="idx-line">
              <b>{progress ? progress.item : "Starting…"}</b>
              <span className="nums">
                {progress ? `${formatSize(progress.overall_downloaded)} / ${formatSize(progress.overall_total)}` : ""}
              </span>
            </div>
            <Bar value={overall} label="Download progress" />
          </div>
        </Grp>
      )}

      {phase === "interrupted" && (
        <div className={cancelled ? "muted" : "fr-error"} role={cancelled ? "status" : "alert"}>
          {cancelled ? "Download paused. Resume continues where it stopped." : `Download failed: ${interrupted}. Resume continues where it stopped.`}
        </div>
      )}

      {phase === "indexing" && <Indexing t1={t1} onPause={onPause} />}

      {phase === "indexing" && optMissing.length > 0 && (
        <Grp title="Document parser">
          <Row title="Document parser" desc={`Optional. ${PARSER_DESC}`}>
            <span className="val">{formatSize(parserBytes)}</span>
            <button className="gbtn" onClick={() => models.start(true)}>Download</button>
          </Row>
        </Grp>
      )}

      <div className="actions">
        {(phase === "choose" || phase === "interrupted") && (
          <button className="btn primary" onClick={() => models.start(includeParser)} disabled={!plan}>
            {phase === "interrupted" ? "Resume download" : "Download"}
          </button>
        )}
        {phase === "downloading" && <button className="btn primary" onClick={models.cancel}>Cancel</button>}
        {phase === "indexing" && <button className="btn primary" onClick={onClose}>Done</button>}
        {phase !== "indexing" && <button className="btn quiet" onClick={onClose}>{phase === "downloading" ? "Hide" : "Not now"}</button>}
      </div>
    </section>
  );
}
