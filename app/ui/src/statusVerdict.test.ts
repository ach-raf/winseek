// Run: npm test   (node's built-in runner; Node 22.6+ strips the types)
import { test } from "node:test";
import assert from "node:assert/strict";
import { statusVerdict } from "./statusVerdict.ts";
import type { StatusPage, StatusParserRun, VolumeDetail } from "./types.ts";

const vol = (letter: string, over: Partial<VolumeDetail> = {}): VolumeDetail => ({
  letter, files: 1000, folders: 100, state: "live", done: null, total: null, error: null, fs: "NTFS", monitor: "usn", ...over,
});
const page = (over: {
  tier0?: Partial<StatusPage["tier0"]>; tier1?: Partial<StatusPage["tier1"]>; parser?: Partial<StatusPage["models"]["parser"]>;
}): StatusPage => ({
  tier0: { source: "searchd", service: "running", uptime_s: 3600, volumes: [vol("C"), vol("D")], can_rescan: true, rescan_hint: null, ...over.tier0 },
  tier1: {
    state: "idle", paused_reason: null, error: null, stats: { done: 500, pending: 0, files: 3000, vectors: 500, text_only: 4, failed: 0, skipped: 2 },
    coverage: 0.83, queue: 0, rate: null, eta_s: null, can_rebuild: true, ocr_unavailable: false, waiting: [], failed_reasons: [], skipped_reasons: [],
    ffmpeg: null, index: null, ...over.tier1,
  },
  models: {
    llama_build: "b11588", backend: "cuda12", gpu: "RTX", llama_present: true, model_id: "m", profile: "fast", quant: "Q8_0",
    files: [], server: { running: true, port: 8080, unload_in_s: null, idle_unload_s: 900 },
    pdfium: { build: "7881", present: true, source: "installed", expected_bytes: 1, bytes: 1 },
    parser: {
      model_id: "p", present: true, files: [], server: { running: false, port: null, unload_in_s: null, idle_unload_s: 900 },
      queue: { queued: 0, done: 0, failed: 0, skipped: 0 }, rate: null, eta_s: null, profile: "fast",
      routing: { doc_min: 0.5, doc_margin: 0.1, screenshot_min_chars: 40, provisional: false },
      run: null, live: null, ...over.parser,
    },
  },
});

test("healthy system: one calm line with the index size and meaning coverage", () => {
  const v = statusVerdict(page({}));
  assert.equal(v.tone, "ok");
  assert.equal(v.headline, "Everything is up to date");
  assert.match(v.sub!, /2,000 files indexed on C D/);
  assert.match(v.sub!, /meaning coverage 83\.0%/);
});

test("meaning not set up is not a problem", () => {
  const v = statusVerdict(page({ tier1: { state: "models_missing", coverage: null } }));
  assert.equal(v.tone, "ok");
  assert.match(v.sub!, /search by meaning is not set up/);
});

test("parse run: pages and time left, the file being read as the detail", () => {
  const run: StatusParserRun = { started_unix: 1, done: 23, failed: 2, skipped: 1, left: 3, total: 29, ended_unix: null, avg_rate: 1.2 };
  const v = statusVerdict(page({ parser: { run, live: { path: "C:\\x\\scan.pdf", pages: 2, age_s: 3 }, eta_s: 21 } }));
  assert.equal(v.tone, "busy");
  assert.equal(v.headline, "Reading 26 of 29 pages · about 21 s left");
  assert.equal(v.sub, "Now reading scan.pdf");
});

test("indexing beats nothing, scanning beats indexing? no — parse first, then indexing, then drives", () => {
  const p = page({ tier1: { state: "indexing", queue: 100, eta_s: 60, stats: { done: 100, pending: 100, files: 3000, vectors: 100, text_only: 0, failed: 0, skipped: 0 } } });
  const v = statusVerdict(p);
  assert.equal(v.tone, "busy");
  assert.equal(v.headline, "Indexing in the background — 100 of 200 files · about 1 min left");
});

test("scanning drive with progress", () => {
  const v = statusVerdict(page({ tier0: { volumes: [vol("C", { state: "scanning", done: 4_000_000, total: 6_000_000 }), vol("D")] } }));
  assert.equal(v.tone, "busy");
  assert.equal(v.headline, "Scanning C: — 4,000,000 of 6,000,000 entries");
});

test("an erroring drive is the headline, a second one goes into the detail", () => {
  const v = statusVerdict(page({ tier0: { volumes: [vol("C", { state: "error", error: "USN journal lost" }), vol("D", { state: "error", error: "disk offline" })] } }));
  assert.equal(v.tone, "problem");
  assert.equal(v.headline, "C: could not keep its index up to date");
  assert.match(v.sub!, /1 more drive failing · USN journal lost/);
});

test("service not answering and ffmpeg download failure are problems", () => {
  assert.equal(statusVerdict(page({ tier0: { source: "unavailable" } })).headline, "The name service is not answering");
  const v = statusVerdict(page({ tier1: { ffmpeg: { state: "failed", downloaded: 1, total: 2, error: "timeout" } } }));
  assert.equal(v.tone, "problem");
  assert.match(v.headline, /audio and video tool/);
});

test("paused is a calm state, not a failure", () => {
  const v = statusVerdict(page({ tier1: { state: "paused", paused_reason: "user" } }));
  assert.equal(v.tone, "ok");
  assert.match(v.headline, /paused/);
});
