import { test } from "node:test";
import assert from "node:assert/strict";
import { activeJobs, jobsFrac, parserBusy } from "./jobsLogic.ts";
import type { StatusPage, StatusParser, Tier1Status, VolumeStatus } from "./types.ts";

const scan = (over: Partial<VolumeStatus> = {}): VolumeStatus =>
  ({ letter: "C", files: 0, folders: 0, state: "scanning", done: 300, total: 900, ...over });

const t1 = (state: Tier1Status["state"], over: Partial<Tier1Status> = {}): Tier1Status =>
  ({
    state,
    stats: { files: 3880, pending: 880, done: 3000, failed: 0, skipped: 0, vectors: 3000, text_only: 0 },
    include: [], exclude: [], error: null, paused_reason: null,
    ...over,
  });

const endedParser = () =>
  parser({ run: { started_unix: 0, done: 36, failed: 0, skipped: 2, left: 0, total: 38, ended_unix: 1000, avg_rate: null }, live: null });

const parser = (over: Partial<StatusParser> = {}): StatusParser =>
  ({
    model_id: "p", present: true, files: [],
    server: { running: true, port: 51235, unload_in_s: null, idle_unload_s: 120 },
    queue: { queued: 0, done: 24, failed: 0, skipped: 2 }, rate: 0.15, eta_s: 20,
    profile: "balanced",
    routing: { doc_min: 0.6, doc_margin: 0.04, screenshot_min_chars: 200, provisional: false },
    run: { started_unix: 0, done: 24, failed: 0, skipped: 2, left: 3, total: 29, ended_unix: null, avg_rate: null },
    live: { path: "C:\\Users\\you\\Documents\\receipts\\invoice-4471.pdf", pages: 3, age_s: 4 },
    ...over,
  });

/** Only the fields the jobs bar reads are real; the rest of the big status answer is filler. */
const page = (over: { rate?: number | null; eta_s?: number | null; parser?: StatusParser; ffmpeg?: StatusPage["tier1"]["ffmpeg"] } = {}): StatusPage =>
  ({
    tier0: {},
    tier1: {
      state: "indexing", paused_reason: null, error: null,
      stats: { files: 3880, pending: 0, done: 3880, failed: 0, skipped: 0, vectors: 3880, text_only: 0 },
      coverage: 1, queue: 0, rate: over.rate ?? null, eta_s: over.eta_s ?? null,
      can_rebuild: true, ocr_unavailable: false, waiting: [], failed_reasons: [], skipped_reasons: [],
      ffmpeg: over.ffmpeg ?? null, index: null,
    },
    models: {
      llama_build: "", backend: "cpu", gpu: null, llama_present: true, model_id: "m", profile: "balanced", quant: "q8",
      files: [], pdfium: { build: "", present: true, source: "installed", expected_bytes: 1, bytes: 1 },
      server: { running: false, port: null, unload_in_s: null, idle_unload_s: 120 },
      parser: over.parser ?? endedParser(),
      reranker: {},
    },
  }) as unknown as StatusPage;

test("idle answers produce no jobs", () => {
  const idle = { ...page(), tier1: { ...page().tier1, state: "idle" as const } };
  assert.deepEqual(activeJobs([], t1("idle"), idle, { running: false, progress: null, failed: false }), []);
});

test("a scanning volume shows letters, counts and a fraction", () => {
  const [job] = activeJobs([scan()], t1("idle"), null, null);
  assert.equal(job!.key, "scan");
  assert.equal(job!.label, "Scanning C · 300 / 900 files");
  assert.ok(Math.abs(job!.frac! - 1 / 3) < 1e-9);
});

test("a rescan says Rescanning and several volumes list every letter", () => {
  const [job] = activeJobs(
    [scan({ state: "rescanning", letter: "D" }), scan({ state: "rescanning", letter: "E", done: 10, total: 100 })],
    t1("idle"), null, null,
  );
  assert.equal(job!.label, "Rescanning D, E · 310 / 1,000 files");
});

test("scan counts without totals stay indeterminate", () => {
  const [job] = activeJobs([scan({ done: null, total: null })], t1("idle"), null, null);
  assert.equal(job!.label, "Scanning C");
  assert.equal(job!.frac, null);
});

test("the meaning index job carries files left, rate and ETA", () => {
  const [job] = activeJobs([], t1("indexing"), page({ rate: 12.4, eta_s: 71 }), null);
  assert.equal(job!.key, "meaning");
  assert.equal(job!.label, "Indexing for search by meaning · 880 files left · 12 files/s · about 1 min left");
  assert.ok(Math.abs(job!.frac! - 3000 / 3880) < 1e-9);
});

test("the meaning job renders without rate or ETA before the status page answers", () => {
  const [job] = activeJobs([], t1("indexing"), null, null);
  assert.equal(job!.label, "Indexing for search by meaning · 880 files left");
});

test("a parser run that has not drained shows pages left, the file in flight and the rate", () => {
  const [job] = activeJobs([], t1("idle"), page({ parser: parser() }), null);
  assert.equal(job!.key, "parse");
  assert.equal(job!.label, "Reading documents · 3 pages left · invoice-4471.pdf · 9.0 pages/min");
  assert.ok(Math.abs(job!.frac! - 26 / 29) < 1e-9);
});

test("a drained parser run is not a job", () => {
  const ended = parser({ run: { started_unix: 0, done: 36, failed: 0, skipped: 2, left: 0, total: 38, ended_unix: 1000, avg_rate: null }, live: null });
  assert.equal(activeJobs([], t1("idle"), page({ parser: ended }), null).length, 0);
});

test("a running model download shows the setup line with its percent", () => {
  const [job] = activeJobs([], t1("idle"), null, { running: true, progress: { overall_downloaded: 430, overall_total: 1000 }, failed: false });
  assert.equal(job!.key, "models");
  assert.equal(job!.label, "Setting up search by meaning · 43.0%");
  assert.equal(job!.frac, 0.43);
});

test("a model download without totals stays indeterminate", () => {
  const [job] = activeJobs([], t1("idle"), null, { running: true, progress: null, failed: false });
  assert.equal(job!.label, "Setting up search by meaning…");
  assert.equal(job!.frac, null);
});

test("the audio/video tool download shows while it runs, not when idle", () => {
  const [job] = activeJobs([], t1("idle"), page({ ffmpeg: { state: "downloading", downloaded: 5, total: 10, error: null } }), null);
  assert.equal(job!.key, "ffmpeg");
  assert.equal(job!.label, "Downloading the audio / video tool · 50.0%");
  assert.deepEqual(activeJobs([], t1("idle"), page({ ffmpeg: { state: "idle", downloaded: 10, total: 10, error: null } }), null), []);
});

test("segments keep a fixed order: scan, meaning, parse, models, ffmpeg", () => {
  const jobs = activeJobs(
    [scan()],
    t1("indexing"),
    page({ parser: parser(), ffmpeg: { state: "downloading", downloaded: 1, total: 4, error: null } }),
    { running: true, progress: { overall_downloaded: 1, overall_total: 2 }, failed: false },
  );
  assert.deepEqual(jobs.map((j) => j.key), ["scan", "meaning", "parse", "models", "ffmpeg"]);
  assert.equal(jobsFrac(jobs), 1 / 3); // the first reported fraction feeds the hairline
});

test("parserBusy mirrors an unfinished run and only that", () => {
  assert.equal(parserBusy(page({ parser: parser() })), true);
  const ended = parser({ run: { started_unix: 0, done: 36, failed: 0, skipped: 2, left: 0, total: 38, ended_unix: 1000, avg_rate: null } });
  assert.equal(parserBusy(page({ parser: ended })), false);
  assert.equal(parserBusy(null), false);
});
