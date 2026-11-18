/** Pure jobs-bar wording: the bottom strip that tells the user what the app is working on right
 *  now. `activeJobs` derives the running jobs from answers the App already polls (volumes, the
 *  light tier-1 status) plus the shared status-page answer (parser run, rate, the audio/video
 *  tool). The bar renders the labels, shows the first reported fraction as a hairline, and
 *  flashes FINISHED_TEXT once a job drains. Kept React-free so the wording is testable. */

import { formatDuration, formatPages, formatPercent, formatRate } from "./format.ts";
import { fileName } from "./reasonText.ts";
import type { ModelsProgress, StatusPage, Tier1Status, VolumeStatus } from "./types.ts";

const fmt = (n: number) => n.toLocaleString("en-US");

export type JobKey = "scan" | "meaning" | "parse" | "models" | "ffmpeg";

export interface JobLine {
  key: JobKey;
  label: string;
  /** 0..1 when the job reports a progress fraction, else null (indeterminate). */
  frac: number | null;
}

/** Shown for a few seconds when a job that was running drains. */
export const FINISHED_TEXT: Record<JobKey, string> = {
  scan: "Drive scan finished",
  meaning: "Search by meaning is up to date",
  parse: "Document reading finished",
  models: "Search by meaning is ready",
  ffmpeg: "Audio / video tool is ready",
};

/** What JobsBar passes for the model download (from the event-fed `useModels`). `failed` is the
 *  cancelled / failed flag: a download that stops that way must not flash "ready". */
export interface ModelsJob {
  running: boolean;
  progress: Pick<ModelsProgress, "overall_downloaded" | "overall_total"> | null;
  failed: boolean;
}

/** The jobs running right now, in fixed display order (scan, meaning index, documents,
 *  downloads) so segments never jump around while several run at once. */
export function activeJobs(
  volumes: VolumeStatus[] | null,
  t1: Tier1Status | null,
  page: StatusPage | null,
  models: ModelsJob | null,
): JobLine[] {
  const jobs: JobLine[] = [];

  // Tier 0: a volume walk (the first scan after setup, or a manual rescan).
  const busy = (volumes ?? []).filter((v) => v.state === "scanning" || v.state === "rescanning");
  if (busy.length) {
    const word = busy.every((v) => v.state === "rescanning") ? "Rescanning" : "Scanning";
    const letters = busy.map((v) => v.letter).join(", ");
    const done = busy.reduce((a, v) => a + (v.done ?? 0), 0);
    const total = busy.reduce((a, v) => a + (v.total ?? 0), 0);
    const counts = total > 0 ? ` · ${fmt(done)} / ${fmt(total)} files` : "";
    jobs.push({ key: "scan", label: `${word} ${letters}${counts}`, frac: total > 0 ? done / total : null });
  }

  // Tier 1: embeddings for search by meaning. Rate / ETA live on the status page; the label
  // renders without them until the first answer arrives (a second or two at most).
  if (t1?.state === "indexing" && t1.stats) {
    const { done, pending } = t1.stats;
    const whole = done + pending;
    const rate = page?.tier1.rate != null ? ` · ${formatRate(page.tier1.rate)}` : "";
    const eta = page?.tier1.eta_s != null ? ` · about ${formatDuration(page.tier1.eta_s)} left` : "";
    jobs.push({
      key: "meaning",
      label: `Indexing for search by meaning · ${fmt(pending)} files left${rate}${eta}`,
      frac: whole > 0 ? done / whole : null,
    });
  }

  // The document parser: an unfinished run (`ended_unix` is stamped only when the queue drains).
  const run = page?.models.parser.run;
  if (run && run.ended_unix == null && run.left > 0) {
    const live = page?.models.parser.live;
    const rate = page?.models.parser.rate != null ? ` · ${formatPages(page.models.parser.rate)}` : "";
    const reading = live ? ` · ${fileName(live.path)}` : "";
    jobs.push({
      key: "parse",
      label: `Reading documents · ${fmt(run.left)} page${run.left === 1 ? "" : "s"} left${reading}${rate}`,
      frac: run.total > 0 ? (run.total - run.left) / run.total : null,
    });
  }

  // Component downloads (embedding models / PDF reader / parser build), fed by events.
  if (models?.running) {
    const p = models.progress;
    const total = p?.overall_total ?? 0;
    jobs.push({
      key: "models",
      label: total > 0 && p ? `Setting up search by meaning · ${formatPercent(p.overall_downloaded / total)}` : "Setting up search by meaning…",
      frac: total > 0 && p ? Math.min(1, p.overall_downloaded / total) : null,
    });
  }

  // The audio / video tool's own downloader (only visible while files wait for it).
  const ff = page?.tier1.ffmpeg;
  if (ff?.state === "downloading") {
    jobs.push({
      key: "ffmpeg",
      label: ff.total > 0 ? `Downloading the audio / video tool · ${formatPercent(ff.downloaded / ff.total)}` : "Downloading the audio / video tool…",
      frac: ff.total > 0 ? Math.min(1, ff.downloaded / ff.total) : null,
    });
  }

  return jobs;
}

/** The fraction the hairline under the strip shows: the first job that reports one. */
export const jobsFrac = (jobs: JobLine[]): number | null => jobs.find((j) => j.frac != null)?.frac ?? null;

/** True while the document parser has pages left — the one running-job signal that lives only on
 *  the status page, so the shared cache entry polls fast exactly while it matters. */
export function parserBusy(page: StatusPage | null | undefined): boolean {
  const run = page?.models.parser.run;
  return !!run && run.ended_unix == null && run.left > 0;
}
