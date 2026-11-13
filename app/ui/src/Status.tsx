import { useCallback, useEffect, useState } from "react";
import type { Backend, StatusModelFile, StatusModels, StatusPage, StatusParser, StatusReason, StatusTier0, StatusFfmpeg, StatusTier1, StatusWaiting, VolumeDetail } from "./types";
import { formatDuration, formatPages, formatPercent, formatRate, formatSize } from "./format";
import { pausedText } from "./Setup";
import { Grp, Row } from "./settingsUi";

const fmt = (n: number) => n.toLocaleString("en-US");
const POLL_MS = 1000;

/** Polls `statusPage` every second while this is mounted and the window is visible. */
export function useStatusPage(backend: Backend): [StatusPage | null, string | null, () => void] {
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

export function Bar({ value, label }: { value: number | null; label: string }) {
  const pct = value == null ? 0 : Math.max(0, Math.min(100, value * 100));
  return (
    <div className={"bar" + (value == null ? " indet" : "")} role="progressbar" aria-label={label}
         aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
      <i style={{ width: pct + "%" }} />
    </div>
  );
}

/** A read-only key/value row (label left, value right). */
function Kv({ k, children, title }: { k: string; children: React.ReactNode; title?: string }) {
  return (
    <div className="grow kv" title={title}>
      <div className="gtxt"><b>{k}</b></div>
      <span className="val">{children}</span>
    </div>
  );
}

/** "exFAT · folder watch" / "NTFS · USN journal"; empty when searchd did not say. */
export const volKind = (v: VolumeDetail) =>
  !v.fs ? "" : v.monitor === "folder_watch" ? `${v.fs} · folder watch` : v.monitor === "usn" ? `${v.fs} · USN journal` : v.fs;

const volState = (v: VolumeDetail) =>
  v.state === "live" ? "Live" : v.state === "scanning" ? "Scanning" : v.state === "rescanning" ? "Rescanning" : v.state === "error" ? "Error" : "Demo data";

export function Names({ t0, busy, onRescan }: { t0: StatusTier0; busy: Set<string>; onRescan: (letter: string) => void }) {
  const head =
    t0.source === "searchd" ? `Running${t0.uptime_s != null ? ` for ${formatDuration(t0.uptime_s)}` : ""}`
    : t0.source === "demo" ? `${t0.service === "not_installed" ? "Not installed" : t0.service === "stopped" ? "Stopped" : "Not running"}. Showing demo data.`
    : "Not answering";
  return (
    <Grp title="File names" label="Names">
      <Row title="Name service" desc={
        <>
          <span className={"st-head " + (t0.source === "searchd" ? "s-live" : "s-warn")}><span className="dot" />{head}</span>
          {t0.source === "unavailable" && <span className="gline">Service state: {t0.service.replace("_", " ")}. This page keeps retrying.</span>}
        </>
      } />
      {t0.volumes.map((v) => {
        const working = v.state === "scanning" || v.state === "rescanning";
        const off = !t0.can_rescan || working || busy.has(v.letter);
        return (
          <div key={v.letter} className={"grow wrap s-" + v.state}>
            <div className="gtxt">
              <b>{v.letter}:</b>
              <span title={v.monitor === "folder_watch" ? "No change journal on this filesystem: changes are watched and the drive is rescanned when the watch overflows" : undefined}>
                {volKind(v) ? `${volKind(v)} · ` : ""}
                {v.state === "scanning" && v.total ? `${fmt(v.done ?? 0)} of ${fmt(v.total)} entries`
                  : v.state === "scanning" && v.done ? `${fmt(v.done)} entries`
                  : v.state === "error" ? "—" : `${fmt(v.files)} files · ${fmt(v.folders)} folders`}
              </span>
            </div>
            <div className="gctl">
              <span className="st-state"><span className="dot" />{volState(v)}</span>
              <button className="gbtn" disabled={off} onClick={() => onRescan(v.letter)}
                      title={!t0.can_rescan ? (t0.rescan_hint ?? "") : working ? "Already scanning" : `Rebuild the index of ${v.letter}: from scratch`}
                      aria-label={`Rescan ${v.letter}:`}>Rescan</button>
            </div>
            {v.state === "scanning" && v.total ? <div className="gsub"><Bar value={v.done != null ? v.done / v.total : null} label={`Scanning ${v.letter}:`} /></div> : null}
            {v.error && <div className="gsub"><span className="fr-error" role="alert">{v.error}</span></div>}
          </div>
        );
      })}
      {t0.volumes.length === 0 && <div className="grow none">No volumes to show</div>}
    </Grp>
  );
}

const plural = (n: number, one: string, many: string) => `${fmt(n)} ${n === 1 ? one : many}`;

/** Where the automatic download of the audio / video tool stands, in plain words. */
function ffmpegText(f: StatusFfmpeg | null): string {
  if (f?.state === "downloading") {
    const pct = f.total > 0 ? ` (${Math.min(99, Math.floor((f.downloaded / f.total) * 100))}%)` : "";
    return `waiting for the audio and video tool, which is downloading${pct}`;
  }
  if (f?.state === "failed") return `waiting for the audio and video tool: the download failed${f.error ? ` (${f.error})` : ""}. Check the internet connection, then press Retry`;
  return "waiting for the audio and video tool, which downloads by itself";
}

/** What one group of queued files is waiting for, in plain words. */
function waitingLine(w: StatusWaiting, f: StatusFfmpeg | null): string {
  return w.why === "pdf_reader_missing" ? `${plural(w.count, "PDF is", "PDFs are")} waiting for the PDF reader to download`
    : w.why === "pdf_reader_broken" ? `${plural(w.count, "PDF is", "PDFs are")} waiting: the PDF reader is installed but could not be loaded`
    : w.why === "ffmpeg_missing" ? `${plural(w.count, w.kind === "audio" ? "audio file is" : "video is", w.kind === "audio" ? "audio files are" : "videos are")} ${ffmpegText(f)}`
    : `${plural(w.count, "file is", "files are")} waiting`;
}

/** The State line while files are queued and nothing is running: why, in plain words. */
export function waitingText(t: StatusTier1): string {
  const blocked = t.waiting.reduce((a, w) => a + w.count, 0);
  if (t.waiting.length > 0 && blocked >= t.queue) return "Waiting. " + t.waiting.map((w) => waitingLine(w, t.ffmpeg)).join("; ");
  if (t.error) return `Retrying after an error (${fmt(t.queue)} queued)`;
  const rest = t.queue - blocked;
  return t.waiting.length > 0
    ? `Starting the next batch of ${fmt(rest)} files. ${t.waiting.map((w) => waitingLine(w, t.ffmpeg)).join("; ")}`
    : `Starting the next batch (${fmt(t.queue)} queued)`;
}

/** Plain-words name for a skip reason code or a failure message. */
export function reasonLabel(r: string): string {
  switch (r) {
    case "": return "No reason recorded";
    case "no_ffmpeg": return "Audio and video: ffmpeg is not installed (they are queued again once it is)";
    case "cloud_only": return "Online-only cloud files (not downloaded to this PC)";
    case "no_heic_codec": return "HEIC photos: the Windows HEIF codec is not installed";
    case "too_large": return "Too large to read";
    case "binary": return "Binary file, no text inside";
    case "no_text": return "No readable text";
    case "legacy_office": return "Old Office format (.doc, .xls, .ppt)";
    case "encrypted": return "Encrypted or password-protected";
    case "no_audio_model": return "Audio: the model has no audio support";
    case "no_media_content": return "Audio or video with no usable sound or picture";
    case "denied": return "Credential or system file (never read)";
    case "zip_bomb": return "Archive that expands suspiciously large";
    case "changed": return "Changed while being read";
    case "not_notebook": return "Not a valid notebook";
    default: return r;
  }
}

/** A count with the reasons behind it: hover shows the first, the arrow opens every reason with example files. */
function Reasons({ k, total, groups, title }: { k: string; total: number | null; groups: StatusReason[]; title: string }) {
  if (total == null || total === 0 || groups.length === 0) return <Kv k={k} title={title}>{total == null ? "—" : fmt(total)}</Kv>;
  const shown = groups.reduce((a, g) => a + g.count, 0);
  return (
    <details className="reasons">
      <summary className="grow kv" title={groups.map((g) => `${fmt(g.count)}  ${reasonLabel(g.reason)}`).join("\n")}>
        <div className="gtxt"><b>{k}</b></div>
        <span className="val">{fmt(total)}</span>
      </summary>
      <ul>
        {groups.map((g) => (
          <li key={g.reason} title={g.examples.join("\n")}>
            <span className="n">{fmt(g.count)}</span>
            <span className="why">{reasonLabel(g.reason)}
              {g.examples.length > 0 && <small>{g.examples.map((p) => p.split(/[\\/]/).pop()).join(", ")}</small>}
            </span>
          </li>
        ))}
        {total > shown && <li><span className="n">{fmt(total - shown)}</span><span className="why">Other reasons</span></li>}
      </ul>
    </details>
  );
}

function stateLine(t: StatusTier1): string {
  return t.state === "unavailable" ? "Unavailable"
    : t.state === "models_missing"
      ? "Not set up. The model files are not downloaded yet." + (t.stats?.text_only ? " Text inside files is already searchable." : "")
    : t.paused_reason ? pausedText(t.paused_reason)
    : t.state === "indexing" ? "Indexing in the background"
    : t.queue > 0 ? waitingText(t) : "All caught up";
}

export function StatusMeaning({ t1, onRebuild, rebuilding, onResume, onRetryFfmpeg }: { t1: StatusTier1; onRebuild: () => void; rebuilding: boolean; onResume?: () => void; onRetryFfmpeg?: () => void }) {
  const [ask, setAsk] = useState(false);
  const s = t1.stats;
  useEffect(() => { if (!t1.can_rebuild) setAsk(false); }, [t1.can_rebuild]);
  return (
    <Grp title="Search by meaning" label="Meaning">
      <Row title="State" desc={<span className={"st-head " + (t1.state === "indexing" || t1.state === "idle" ? "s-live" : "s-warn")}><span className="dot" />{stateLine(t1)}</span>}>
        {t1.paused_reason === "user" && onResume && <button className="gbtn pri" onClick={onResume}>Resume</button>}
        {t1.ffmpeg?.state === "failed" && onRetryFfmpeg && <button className="gbtn pri" onClick={onRetryFfmpeg}>Retry</button>}
      </Row>
      {t1.state === "unavailable" ? (
        t1.error && <div className="grow"><span className="fr-error" role="alert">{t1.error}</span></div>
      ) : (
        <>
          <div className="grow stack">
            <div className="idx-line">
              <b>Coverage</b>
              <span className="nums">{t1.coverage == null ? "—" : formatPercent(t1.coverage)}</span>
            </div>
            <Bar value={t1.coverage} label="Indexing coverage" />
          </div>
          <Kv k="Embedded">{s ? fmt(s.done) : "—"}</Kv>
          <Kv k="Queue" title="Files waiting to be embedded">{s ? fmt(t1.queue) : "—"}</Kv>
          <Kv k="Text searchable, waiting for models" title="Their text is already searchable; the meaning vectors are added once the models are installed">
            {s ? fmt(s.text_only) : "—"}
          </Kv>
          <Reasons k="Failed" total={s ? s.failed : null} groups={t1.failed_reasons} title="Files that could not be read or embedded" />
          <Reasons k="Skipped" total={s ? s.skipped : null} groups={t1.skipped_reasons} title="Unsupported or legacy format, too large, no text, or ignored" />
          <Kv k="Files in scope">{s ? fmt(s.files) : "—"}</Kv>
          <Kv k="Speed" title="Files per second over the last minute">{t1.rate != null ? formatRate(t1.rate) : "—"}</Kv>
          <Kv k="Screenshot text recognition" title="Reads text in screenshots with Windows OCR">
            {t1.ocr_unavailable ? "unavailable — install a Windows OCR language (Settings › Time & language › Language)" : "on"}
          </Kv>
          <Kv k="Time left">{t1.eta_s != null ? `about ${formatDuration(t1.eta_s)}` : "—"}</Kv>
          {t1.error && <div className="grow"><span className="fr-error" role="alert">Last error: {t1.error}</span></div>}
          {!ask ? (
            <Row title="Rebuild meaning index" desc="Forget every embedding and index the folders again">
              <button className="gbtn" disabled={!t1.can_rebuild || rebuilding} onClick={() => setAsk(true)}
                      title={t1.can_rebuild ? "Forget every embedding and index the folders again" : "Search by meaning is not available"}>
                {rebuilding ? "Rebuilding…" : "Rebuild…"}
              </button>
            </Row>
          ) : (
            <div className="grow wrap" role="alertdialog" aria-label="Confirm rebuild">
              <div className="gtxt"><b>Delete the meaning index and embed every file again?</b><span>This can take a while. Name search is not affected.</span></div>
              <div className="gctl">
                <button className="gbtn pri" autoFocus onClick={() => { setAsk(false); onRebuild(); }}>Rebuild</button>
                <button className="gbtn" onClick={() => setAsk(false)}>Cancel</button>
              </div>
            </div>
          )}
        </>
      )}
    </Grp>
  );
}

const backendName = (m: StatusModels) => (m.backend === "cuda12" ? "CUDA 12" : m.backend === "vulkan" ? "Vulkan" : "CPU");

function Files({ files }: { files: StatusModelFile[] }) {
  return (
    <>
      {files.map((f) => (
        <Kv key={f.name} k={`${f.role === "model" ? "Model" : "Vision projector"} · ${f.name}`}>
          <span className={f.bytes == null ? "st-miss" : undefined}>
            {f.bytes == null ? `Missing (${formatSize(f.expected_bytes)})` : f.bytes === f.expected_bytes ? formatSize(f.bytes) : `${formatSize(f.bytes)} of ${formatSize(f.expected_bytes)}`}
          </span>
        </Kv>
      ))}
    </>
  );
}

const profileName = (p: StatusParser["profile"]) => p.charAt(0).toUpperCase() + p.slice(1);

/** The document parser: files, server, queue, profile and the routing thresholds. */
function ParserRows({ p }: { p: StatusParser }) {
  const sv = p.server;
  const q = p.queue;
  const r = p.routing;
  return (
    <Grp title="Document parser">
      <Kv k="Document parser" title="Infinity-Parser2 reads scans, receipts and screenshots into text and tables">
        {p.model_id}
        {p.present ? " · installed" : <span className="st-miss"> · not installed (optional, add it in Setup)</span>}
      </Kv>
      <Kv k="Parser server">
        {sv.running
          ? `Running${sv.port != null ? ` on port ${sv.port}` : ""}${sv.unload_in_s != null ? ` · frees GPU memory in ${formatDuration(sv.unload_in_s)} if idle` : ""}`
          : `Not running. It starts when needed and unloads after ${formatDuration(sv.idle_unload_s)} idle.`}
      </Kv>
      <Kv k="Parse queue" title="Images and scanned pages waiting for, finished by, skipped by, or given up on by the document parser">
        {fmt(q.queued)} queued · {fmt(q.done)} done · {fmt(q.skipped)} skipped · {fmt(q.failed)} failed
      </Kv>
      <Kv k="Parse speed" title="Pages per minute over the last minute, and how long the queue still needs">
        {p.rate != null
          ? `${formatPages(p.rate)}${p.eta_s != null && q.queued > 0 ? ` · about ${formatDuration(p.eta_s)} left` : ""}`
          : "—"}
      </Kv>
      <Kv k="Profile" title="Performance profile (Settings)">{profileName(p.profile)}</Kv>
      <Kv k="Parser routing thresholds" title="Which images go to the parser. Tuned = measured on a labelled set of documents and photos.">
        {r.provisional ? "provisional" : "tuned"} · document score ≥ {r.doc_min}, margin ≥ {r.doc_margin}, screenshots from {r.screenshot_min_chars} characters
      </Kv>
      <Files files={p.files} />
    </Grp>
  );
}

export function Models({ m }: { m: StatusModels }) {
  const sv = m.server;
  return (
    <>
      <Grp title="Models" label="Models">
        <Kv k="llama.cpp">
          {m.llama_build} · {backendName(m)}{m.gpu ? ` · ${m.gpu}` : ""}
          {!m.llama_present && <span className="st-miss"> · not installed</span>}
        </Kv>
        <Kv k="PDF engine" title="pdfium reads PDF text and draws the pages for scans and the preview">
          pdfium {m.pdfium.build}
          {m.pdfium.present
            ? ` · ${m.pdfium.source === "override" ? "dev override" : "installed"}${m.pdfium.bytes != null ? ` (${formatSize(m.pdfium.bytes)})` : ""}`
            : <span className="st-miss"> · not installed (downloads when a PDF is found, {formatSize(m.pdfium.expected_bytes)})</span>}
        </Kv>
        <Kv k="Embedder">{m.model_id} · {m.profile} profile · {m.quant}</Kv>
        <Kv k="Server">
          {sv.running
            ? `Running${sv.port != null ? ` on port ${sv.port}` : ""}${sv.unload_in_s != null ? ` · frees GPU memory in ${formatDuration(sv.unload_in_s)} if idle` : ""}`
            : `Not running. It starts when needed and unloads after ${formatDuration(sv.idle_unload_s)} idle.`}
        </Kv>
        <Files files={m.files} />
      </Grp>
      <ParserRows p={m.parser} />
    </>
  );
}
