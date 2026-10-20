import { useEffect, useState } from "react";
import type { ServiceState, VolumeStatus } from "./types";
import { trapKey } from "./trap";

export interface Summary { files: number; letters: string[]; secs: number | null }

const fmt = (n: number) => n.toLocaleString("en-US");

function Shield() {
  // UAC-style shield: blue/yellow quadrants
  return (
    <svg className="shield" viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
      <defs><clipPath id="shield-clip"><path d="M12 2 21 5v6c0 5.4-3.8 9.4-9 11-5.2-1.6-9-5.6-9-11V5z" /></clipPath></defs>
      <g clipPath="url(#shield-clip)">
        <rect x="0" y="0" width="12" height="12" fill="#ffc83d" />
        <rect x="12" y="0" width="12" height="12" fill="#2f8bf0" />
        <rect x="0" y="12" width="12" height="12" fill="#2f8bf0" />
        <rect x="12" y="12" width="12" height="12" fill="#ffc83d" />
      </g>
      <path d="M12 2 21 5v6c0 5.4-3.8 9.4-9 11-5.2-1.6-9-5.6-9-11V5z" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" />
    </svg>
  );
}

function useCountUp(target: number): number {
  const [shown, setShown] = useState(target);
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) { setShown(target); return; }
    let raf = 0;
    const tick = () => {
      let done = false;
      setShown((cur) => {
        const d = target - cur;
        if (Math.abs(d) < 1) { done = true; return target; }
        return cur + Math.sign(d) * Math.max(1, Math.abs(d) * 0.2);
      });
      if (!done) raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [target]);
  return Math.round(shown);
}

function Counter({ rows, summary, onSemantic }: { rows: VolumeStatus[]; summary: Summary | null; onSemantic?: () => void }) {
  const total = rows.reduce((a, v) => a + (v.state === "scanning" && v.done != null ? v.done : v.files + v.folders), 0);
  const shown = useCountUp(summary ? summary.files : total);
  return (
    <>
      <h2>{summary ? "Name service is ready" : rows.length ? "Indexing your drives" : "Starting the name service"}</h2>
      <div className="bigcount" aria-hidden="true">{fmt(shown)}<small>files and folders</small></div>
      {rows.length === 0 && <div className="vols"><div className="bar indet" role="progressbar" aria-label="Starting"><i /></div></div>}
      <div className="vols">
        {rows.map((v) => {
          const scanning = v.state === "scanning";
          const known = scanning && v.done != null && v.total ? v.done / v.total : null;
          const pct = scanning ? (known ?? 0) * 100 : 100;
          const n = v.files + v.folders;
          return (
            <div className="vol" key={v.letter}>
              <span className="letter">{v.letter}:</span>
              <span className="nums">{scanning ? (v.done != null && v.total ? `${fmt(v.done)} / ${fmt(v.total)}` : "Scanning…") : fmt(n)}</span>
              <span />
              <div className={"bar" + (scanning && known == null ? " indet" : "")} role="progressbar" aria-label={`Volume ${v.letter}:`}
                   aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
                <i style={{ width: pct + "%" }} />
              </div>
            </div>
          );
        })}
      </div>
      {summary && (
        <p className="summary" role="status">
          Indexed <b>{fmt(summary.files)} files</b> across {summary.letters.map((l) => l + ":").join(", ")}
          {summary.secs != null && <> in {summary.secs < 1 ? "<1" : Math.round(summary.secs)} s</>} · Start typing
        </p>
      )}
      {summary && onSemantic && (
        <div className="actions">
          <span className="muted">Want search by meaning?</span>
          <button className="btn quiet" onClick={onSemantic}>Set up</button>
        </div>
      )}
    </>
  );
}

export function FirstRun({ mode, service, busy, error, rows, summary, onPrimary, onDismiss, onSemantic }: {
  mode: "setup" | "counter"; service: ServiceState | null; busy: boolean; error: string | null;
  rows: VolumeStatus[]; summary: Summary | null; onPrimary: () => void; onDismiss: () => void;
  /** Offered after the counter finishes while search by meaning is not set up. */
  onSemantic?: () => void;
}) {
  const stopped = service === "stopped";
  return (
    <section
      className="firstrun"
      aria-label="Name service setup"
      onKeyDown={(e) => {
        if (mode !== "setup") return;
        if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); if (!busy) onDismiss(); }
        else trapKey(e);
      }}
    >
      {mode === "counter" ? <Counter rows={rows} summary={summary} onSemantic={onSemantic} /> : (
        <>
          <h2>{stopped ? "Name service is stopped" : "Search every file by name, instantly"}</h2>
          <p>
            {stopped
              ? "The winseek name service keeps the file index up to date. Start it to search your drives; no administrator permission is needed."
              : "The name service reads the NTFS file table, like Everything, watches FAT, exFAT and ReFS drives, and runs as a Windows service. Filenames never leave this PC."}
          </p>
          <div className="actions">
            <button className="btn primary" disabled={busy} onClick={onPrimary} aria-busy={busy}>
              {busy ? <span className="spin" aria-hidden="true" /> : !stopped && <Shield />}
              {busy ? (stopped ? "Starting…" : "Waiting for permission…") : stopped ? "Start" : "Install name service"}
            </button>
            <button className="btn quiet" disabled={busy} onClick={onDismiss}>Not now</button>
          </div>
          {error && <div className="fr-error" role="alert">{error}</div>}
        </>
      )}
    </section>
  );
}
