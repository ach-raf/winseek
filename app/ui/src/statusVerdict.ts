/** Pure status-page wording: what the groups in Status.tsx render, said in one place so it can be
 *  tested without React. `statusVerdict` is the one calm line the page opens with. */

import { fileName } from "./reasonText.ts";
import type { PausedReason, StatusFfmpeg, StatusPage, StatusTier1, StatusWaiting } from "./types.ts";
import { formatDuration, formatPercent } from "./format.ts";

const fmt = (n: number) => n.toLocaleString("en-US");
export const plural = (n: number, one: string, many: string) => `${fmt(n)} ${n === 1 ? one : many}`;

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

export function pausedText(r: PausedReason): string {
  return r === "user" ? "Paused by you"
    : r === "battery" ? "Paused on battery power. It resumes when you plug in."
    : "Paused while a full-screen game or presentation runs. It resumes afterwards.";
}

export interface Verdict { tone: "ok" | "busy" | "problem"; headline: string; sub: string | null }

/** The single thing that needs attention, the work in flight, or a calm confirmation. Presentation
 *  only — every field stays visible in the groups below. Order: one failing thing first, then the
 *  busiest work, then the quiet truth. */
export function statusVerdict(p: StatusPage): Verdict {
  if (p.tier0.source === "unavailable")
    return { tone: "problem", headline: "The name service is not answering", sub: "This page keeps retrying." };
  if (p.tier0.source === "demo")
    return {
      tone: "problem",
      headline: p.tier0.service === "not_installed" ? "The name service is not installed" : p.tier0.service === "stopped" ? "The name service is stopped" : "The name service is not running",
      sub: "Showing demo data.",
    };
  const bad = p.tier0.volumes.filter((v) => v.state === "error");
  if (bad.length > 0)
    return {
      tone: "problem",
      headline: `${bad[0]!.letter}: could not keep its index up to date`,
      sub: (bad.length > 1 ? `${plural(bad.length - 1, "more drive", "more drives")} failing · ` : "") + (bad[0]!.error ?? "Press Rescan to rebuild it."),
    };
  if (p.tier1.state === "unavailable")
    return { tone: "problem", headline: "Search by meaning is unavailable", sub: p.tier1.error ?? "The meaning store could not be opened." };
  if (p.tier1.ffmpeg?.state === "failed")
    return { tone: "problem", headline: "The audio and video tool could not download", sub: (p.tier1.ffmpeg.error ? `${p.tier1.ffmpeg.error} · ` : "") + "Press Retry under Search by meaning." };

  const run = p.models.parser.run;
  if (run && run.ended_unix == null && run.left > 0) {
    const handled = run.done + run.failed + run.skipped;
    return {
      tone: "busy",
      headline: `Reading ${fmt(handled)} of ${fmt(run.total)} pages${p.models.parser.eta_s != null ? ` · about ${formatDuration(p.models.parser.eta_s)} left` : ""}`,
      sub: p.models.parser.live ? `Now reading ${fileName(p.models.parser.live.path)}` : null,
    };
  }
  const s = p.tier1.stats;
  if (p.tier1.state === "indexing" && s)
    return {
      tone: "busy",
      headline: `Indexing in the background — ${fmt(s.done)} of ${fmt(s.done + p.tier1.queue)} files${p.tier1.eta_s != null ? ` · about ${formatDuration(p.tier1.eta_s)} left` : ""}`,
      sub: null,
    };
  const working = p.tier0.volumes.filter((v) => v.state === "scanning" || v.state === "rescanning");
  if (working.length > 0) {
    const v = working[0]!;
    const prog = v.state === "scanning" && v.total != null && v.done != null ? ` — ${fmt(v.done)} of ${fmt(v.total)} entries` : "…";
    return {
      tone: "busy",
      headline: `${v.state === "rescanning" ? "Rescanning" : "Scanning"} ${v.letter}:${prog}`,
      sub: working.length > 1 ? `${plural(working.length - 1, "more drive", "more drives")} in the queue` : null,
    };
  }
  if (p.tier1.state === "paused")
    return { tone: "ok", headline: "Search by meaning is paused", sub: p.tier1.paused_reason ? pausedText(p.tier1.paused_reason) : "Resume under Search by meaning." };
  if (p.tier1.queue > 0)
    return { tone: "busy", headline: waitingText(p.tier1), sub: null };

  const files = p.tier0.volumes.reduce((a, v) => a + v.files, 0);
  const letters = p.tier0.volumes.map((v) => v.letter).join(" ");
  const parts = [`${plural(files, "file", "files")} indexed${letters ? ` on ${letters}` : ""}`];
  if (p.tier1.state === "models_missing") parts.push("search by meaning is not set up");
  else if (p.tier1.coverage != null) parts.push(`meaning coverage ${formatPercent(p.tier1.coverage)}`);
  return { tone: "ok", headline: "Everything is up to date", sub: parts.join(" · ") };
}
