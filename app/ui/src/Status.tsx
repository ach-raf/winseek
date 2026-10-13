import { useCallback, useEffect, useRef, useState } from "react";
import type { Backend, StatusModels, StatusPage, StatusTier0, StatusTier1, VolumeDetail } from "./types";
import { formatDuration, formatPercent, formatRate, formatSize } from "./format";
import { pausedText } from "./Setup";

const fmt = (n: number) => n.toLocaleString("en-US");
const POLL_MS = 1000;

/** Polls `statusPage` every second while this is mounted and the window is visible. */
function useStatusPage(backend: Backend): [StatusPage | null, string | null, () => void] {
  const [page, setPage] = useState<StatusPage | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const load = useCallback(() => {
    backend.statusPage().then((p) => { setPage(p); setErr(null); }).catch((e) => setErr(String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "")));
  }, [backend]);
  useEffect(() => {
    load();
    const t = window.setInterval(() => { if (!document.hidden) load(); }, POLL_MS);
    const onVis = () => { if (!document.hidden) load(); };
    document.addEventListener("visibilitychange", onVis);
    return () => { window.clearInterval(t); document.removeEventListener("visibilitychange", onVis); };
  }, [load]);
  return [page, err, load];
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

function Row({ k, children, title }: { k: string; children: React.ReactNode; title?: string }) {
  return (<><dt>{k}</dt><dd title={title}>{children}</dd></>);
}

const volState = (v: VolumeDetail) =>
  v.state === "live" ? "Live" : v.state === "scanning" ? "Scanning" : v.state === "rescanning" ? "Rescanning" : v.state === "error" ? "Error" : "Demo data";

function Names({ t0, busy, onRescan }: { t0: StatusTier0; busy: Set<string>; onRescan: (letter: string) => void }) {
  const head =
    t0.source === "searchd" ? `Name service running${t0.uptime_s != null ? ` for ${formatDuration(t0.uptime_s)}` : ""}`
    : t0.source === "demo" ? `Name service ${t0.service === "not_installed" ? "not installed" : t0.service === "stopped" ? "stopped" : "not running"}. Showing demo data.`
    : "Name service is not answering";
  return (
    <div className="st-sec" aria-label="Names">
      <div className="fl-title">Names · Tier 0</div>
      <div className={"set-state st-head " + (t0.source === "searchd" ? "s-live" : "s-warn")}><span className="dot" />{head}</div>
      {t0.source === "unavailable" && <div className="muted">Service state: {t0.service.replace("_", " ")}. This page keeps retrying.</div>}
      <ul className="st-vols">
        {t0.volumes.map((v) => {
          const working = v.state === "scanning" || v.state === "rescanning";
          const off = !t0.can_rescan || working || busy.has(v.letter);
          return (
            <li key={v.letter} className={"st-vol s-" + v.state}>
              <span className="letter">{v.letter}:</span>
              <span className="nums">
                {v.state === "scanning" && v.total ? `${fmt(v.done ?? 0)} of ${fmt(v.total)} entries`
                  : v.state === "error" ? "—" : `${fmt(v.files)} files · ${fmt(v.folders)} folders`}
              </span>
              <span className="st-state"><span className="dot" />{volState(v)}</span>
              <button className="btn quiet" disabled={off} onClick={() => onRescan(v.letter)}
                      title={!t0.can_rescan ? (t0.rescan_hint ?? "") : working ? "Already scanning" : `Rebuild the index of ${v.letter}: from scratch`}
                      aria-label={`Rescan ${v.letter}:`}>Rescan</button>
              {v.state === "scanning" && v.total ? <Bar value={v.done != null ? v.done / v.total : null} label={`Scanning ${v.letter}:`} /> : null}
              {v.error && <div className="fr-error st-err" role="alert">{v.error}</div>}
            </li>
          );
        })}
        {t0.volumes.length === 0 && <li className="none">No volumes to show</li>}
      </ul>
    </div>
  );
}

function stateLine(t: StatusTier1): string {
  return t.state === "unavailable" ? "Unavailable"
    : t.state === "models_missing"
      ? "Not set up. The model files are not downloaded yet." + (t.stats?.text_only ? " Text inside files is already searchable." : "")
    : t.paused_reason ? pausedText(t.paused_reason)
    : t.state === "indexing" ? "Indexing in the background"
    : t.queue > 0 ? "Waiting for work" : "All caught up";
}

function Meaning({ t1, onRebuild, rebuilding }: { t1: StatusTier1; onRebuild: () => void; rebuilding: boolean }) {
  const [ask, setAsk] = useState(false);
  const s = t1.stats;
  useEffect(() => { if (!t1.can_rebuild) setAsk(false); }, [t1.can_rebuild]);
  return (
    <div className="st-sec" aria-label="Meaning">
      <div className="fl-title">Meaning · Tier 1</div>
      <div className={"set-state st-head " + (t1.state === "indexing" || t1.state === "idle" ? "s-live" : "s-warn")}><span className="dot" />{stateLine(t1)}</div>
      {t1.state === "unavailable" ? (
        t1.error && <div className="fr-error" role="alert">{t1.error}</div>
      ) : (
        <>
          <div className="st-cov">
            <div className="idx-line">
              <b>Coverage</b>
              <span className="nums">{t1.coverage == null ? "—" : formatPercent(t1.coverage)}</span>
            </div>
            <Bar value={t1.coverage} label="Indexing coverage" />
          </div>
          <dl className="st-grid">
            <Row k="Embedded">{s ? fmt(s.done) : "—"}</Row>
            <Row k="Queue" title="Files waiting to be embedded">{s ? fmt(t1.queue) : "—"}</Row>
            <Row k="Text searchable, waiting for models" title="Their text is already searchable; the meaning vectors are added once the models are installed">
              {s ? fmt(s.text_only) : "—"}
            </Row>
            <Row k="Failed">{s ? fmt(s.failed) : "—"}</Row>
            <Row k="Skipped" title="Unsupported or legacy format, too large, no text, or ignored">{s ? fmt(s.skipped) : "—"}</Row>
            <Row k="Files in scope">{s ? fmt(s.files) : "—"}</Row>
            <Row k="Speed" title="Files per second over the last minute">{t1.rate != null ? formatRate(t1.rate) : "—"}</Row>
            <Row k="Screenshot text recognition" title="Reads text in screenshots with Windows OCR">
              {t1.ocr_unavailable ? "unavailable — install a Windows OCR language (Settings › Time & language › Language)" : "on"}
            </Row>
            <Row k="Time left">{t1.eta_s != null ? `about ${formatDuration(t1.eta_s)}` : "—"}</Row>
          </dl>
          {t1.error && <div className="fr-error" role="alert">Last error: {t1.error}</div>}
        </>
      )}
      <div className="st-act">
        {!ask ? (
          <button className="btn quiet" disabled={!t1.can_rebuild || rebuilding} onClick={() => setAsk(true)}
                  title={t1.can_rebuild ? "Forget every embedding and index the folders again" : "Search by meaning is not available"}>
            {rebuilding ? "Rebuilding…" : "Rebuild meaning index…"}
          </button>
        ) : (
          <div className="st-confirm" role="alertdialog" aria-label="Confirm rebuild">
            <span>Delete the meaning index and embed every file again? This can take a while. Name search is not affected.</span>
            <button className="btn primary" autoFocus onClick={() => { setAsk(false); onRebuild(); }}>Rebuild</button>
            <button className="btn quiet" onClick={() => setAsk(false)}>Cancel</button>
          </div>
        )}
      </div>
    </div>
  );
}

const backendName = (m: StatusModels) => (m.backend === "cuda12" ? "CUDA 12" : m.backend === "vulkan" ? "Vulkan" : "CPU");

function Models({ m }: { m: StatusModels }) {
  const sv = m.server;
  return (
    <div className="st-sec" aria-label="Models">
      <div className="fl-title">Models</div>
      <dl className="st-grid">
        <Row k="llama.cpp">
          {m.llama_build} · {backendName(m)}{m.gpu ? ` · ${m.gpu}` : ""}
          {!m.llama_present && <span className="st-miss"> · not installed</span>}
        </Row>
        <Row k="PDF engine" title="pdfium reads PDF text and draws the pages for scans and the preview">
          pdfium {m.pdfium.build}
          {m.pdfium.present
            ? ` · ${m.pdfium.source === "override" ? "dev override" : "installed"}${m.pdfium.bytes != null ? ` (${formatSize(m.pdfium.bytes)})` : ""}`
            : <span className="st-miss"> · not installed (downloads when a PDF is found, {formatSize(m.pdfium.expected_bytes)})</span>}
        </Row>
        <Row k="Embedder">{m.model_id} · {m.profile} profile · {m.quant}</Row>
        <Row k="Server">
          {sv.running
            ? `Running${sv.port != null ? ` on port ${sv.port}` : ""}${sv.unload_in_s != null ? ` · frees GPU memory in ${formatDuration(sv.unload_in_s)} if idle` : ""}`
            : `Not running. It starts when needed and unloads after ${formatDuration(sv.idle_unload_s)} idle.`}
        </Row>
      </dl>
      <ul className="items st-files">
        {m.files.map((f) => (
          <li key={f.name} className={f.bytes != null ? "have" : ""}>
            <span title={f.name}>{f.role === "model" ? "Model" : "Vision projector"} · {f.name}</span>
            <span className={"sz" + (f.bytes == null ? " st-miss" : "")}>
              {f.bytes == null ? `Missing (${formatSize(f.expected_bytes)})` : f.bytes === f.expected_bytes ? formatSize(f.bytes) : `${formatSize(f.bytes)} of ${formatSize(f.expected_bytes)}`}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function Status({ backend, onClose, onError }: { backend: Backend; onClose: () => void; onError: (e: unknown) => void }) {
  const root = useRef<HTMLElement>(null);
  const [page, err, reload] = useStatusPage(backend);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [rebuilding, setRebuilding] = useState(false);
  useEffect(() => { root.current?.querySelector<HTMLElement>(".actions button")?.focus(); }, []);

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

  return (
    <section ref={root} className="firstrun setup settings statuspage" aria-label="Status"
             onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); } }}>
      <h2>Status</h2>
      {!page ? (
        err ? <div className="fr-error" role="alert">Could not read the status: {err}</div>
          : <div className="vols"><Bar value={null} label="Loading" /><span className="muted">Loading…</span></div>
      ) : (
        <>
          <div className="st-cols">
            <Names t0={page.tier0} busy={busy} onRescan={rescan} />
            <Meaning t1={page.tier1} onRebuild={rebuild} rebuilding={rebuilding} />
          </div>
          <Models m={page.models} />
        </>
      )}
      <div className="actions">
        <span className="muted">Updates every second.</span>
        <button className="btn primary" onClick={onClose}>Done</button>
      </div>
    </section>
  );
}
