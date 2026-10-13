import { useEffect, useRef, useState } from "react";
import type { Models } from "./hooks";
import type { ModelsPlan, PausedReason, Tier1Status } from "./types";
import { formatSize } from "./format";

const fmt = (n: number) => n.toLocaleString("en-US");

export function backendLine(plan: ModelsPlan): string {
  const how = plan.backend === "cuda12" ? "using CUDA" : plan.backend === "vulkan" ? "using Vulkan" : "using the CPU";
  return plan.gpu ? `${plan.gpu}, ${how}` : `No dedicated GPU found, ${how}`;
}

export function pausedText(r: PausedReason): string {
  return r === "user" ? "Paused by you"
    : r === "battery" ? "Paused on battery power. It resumes when you plug in."
    : "Paused while a full-screen app is running. It resumes afterwards.";
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

function Folders({ t1, onChange }: { t1: Tier1Status | null; onChange: (include: string[]) => void }) {
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
  return (
    <div className="folders">
      <div className="fl-title">Folders to index</div>
      <ul>
        {known.map((p) => (
          <li key={p}>
            <label title={p}>
              <input type="checkbox" checked={on(p)} onChange={() => toggle(p)} />
              <span>{p}</span>
            </label>
          </li>
        ))}
        {known.length === 0 && <li className="none">No folders yet</li>}
      </ul>
      <div className="fl-add">
        <input value={text} onChange={(e) => setText(e.target.value)} placeholder="Add a folder, e.g. D:\Photos" aria-label="Add a folder"
               spellCheck={false} autoComplete="off"
               onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); add(); } }} />
        <button className="btn quiet" onClick={add} disabled={!text.trim()}>Add</button>
      </div>
    </div>
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
    <div className="idx">
      <div className="idx-line">
        <b>{starting ? "Starting…" : t1.state === "idle" && total > 0 && s!.pending === 0 ? "All caught up" : t1.state === "paused" ? "Indexing paused" : "Indexing in the background"}</b>
        <span className="nums">{s ? `${fmt(s.done)} of ${fmt(total)} files` : ""}</span>
      </div>
      <Bar value={starting ? null : (frac ?? 1)} label="Indexing progress" />
      {t1.paused_reason && <div className="idx-note" role="status">{pausedText(t1.paused_reason)}</div>}
      {s && s.failed > 0 && <div className="idx-note">{fmt(s.failed)} file{s.failed === 1 ? "" : "s"} could not be read</div>}
      {t1.error && <div className="fr-error" role="alert">{t1.error}</div>}
      {(t1.state === "indexing" || t1.state === "paused") && (t1.paused_reason == null || t1.paused_reason === "user") && (
        <div><button className="btn quiet" onClick={() => onPause(t1.paused_reason == null)}>{t1.paused_reason == null ? "Pause" : "Resume"}</button></div>
      )}
    </div>
  );
}

export function Setup({ t1, models, onFolders, onPause, onClose, primaryRef }: {
  t1: Tier1Status | null; models: Models;
  onFolders: (include: string[]) => void; onPause: (p: boolean) => void; onClose: () => void;
  primaryRef: React.MutableRefObject<() => void>;
}) {
  const { plan, running, progress, interrupted } = models;
  const missing = plan ? plan.items.some((i) => !i.present) : false;
  const phase = running ? "downloading" : !plan ? "loading" : missing ? (interrupted ? "interrupted" : "choose") : "indexing";
  primaryRef.current = () => { if (phase === "choose" || phase === "interrupted") models.start(); else if (phase === "indexing") onClose(); };
  const root = useRef<HTMLElement>(null);
  const overall = progress && progress.overall_total > 0 ? progress.overall_downloaded / progress.overall_total : null;
  const cancelled = interrupted === "cancelled";

  return (
    <section ref={root} className="firstrun setup" aria-label="Search by meaning setup"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } }}>
      <h2>Want search by meaning?</h2>
      <p>Find photos by what is in them, like &ldquo;sunset at the beach&rdquo;. Everything runs on this PC; nothing leaves it. Names-only search stays fully available.</p>

      <div className={phase === "choose" || phase === "interrupted" || phase === "loading" ? "setup-cols" : undefined}>
      {(phase === "choose" || phase === "interrupted" || phase === "loading") && <Folders t1={t1} onChange={onFolders} />}

      {phase === "loading" && <div className="vols"><Bar value={null} label="Detecting your GPU" /><span className="muted">Detecting your GPU…</span></div>}

      {plan && (phase === "choose" || phase === "interrupted" || phase === "downloading") && (
        <div className="dl-plan">
          <div className="gpu">{backendLine(plan)}</div>
          <ul className="items">
            {plan.items.map((i) => (
              <li key={i.name} className={i.present ? "have" : ""}>
                <span>{i.name}</span>
                <span className="sz">{i.present ? "Installed" : i.bytes != null ? formatSize(i.bytes) : "size shown when it starts"}</span>
              </li>
            ))}
          </ul>
          {phase !== "downloading" && (
            <div className="muted">
              Download {formatSize(plan.total_bytes_missing)}{plan.size_unknown ? " plus the llama.cpp runtime" : ""}
            </div>
          )}
        </div>
      )}
      </div>

      {phase === "downloading" && (
        <div className="vols dl-prog">
          <div className="idx-line">
            <b>{progress ? progress.item : "Starting…"}</b>
            <span className="nums">
              {progress ? `${formatSize(progress.overall_downloaded)} / ${formatSize(progress.overall_total)}` : ""}
            </span>
          </div>
          <Bar value={overall} label="Download progress" />
        </div>
      )}

      {phase === "interrupted" && (
        <div className={cancelled ? "muted" : "fr-error"} role={cancelled ? "status" : "alert"}>
          {cancelled ? "Download paused. Resume continues where it stopped." : `Download failed: ${interrupted}. Resume continues where it stopped.`}
        </div>
      )}

      {phase === "indexing" && <Indexing t1={t1} onPause={onPause} />}

      <div className="actions">
        {(phase === "choose" || phase === "interrupted") && (
          <button className="btn primary" onClick={models.start} disabled={!plan}>
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
