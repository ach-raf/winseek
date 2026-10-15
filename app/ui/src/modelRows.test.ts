import { test } from "node:test";
import assert from "node:assert/strict";
import { rowAction, type DownloadState } from "./modelRows.ts";
import type { ModelsPlan, PlanItem } from "./types.ts";

const item = (name: string, group: PlanItem["group"], bytes: number | null, present = false): PlanItem =>
  ({ name, group, bytes, present, optional: group === "parser" });

const plan = (items: PlanItem[]): ModelsPlan => ({
  backend: "cuda12", gpu: null, driver: null, items, total_bytes_missing: 0, size_unknown: false, downloading: false,
});

const base = plan([
  item("llama.cpp", "llama", null, true),
  item("EmbeddingGemma-2 model", "embedder", 300),
  item("EmbeddingGemma-2 vision projector", "embedder", 500),
  item("PDF engine", "pdfium", 4, true),
  item("Document parser model", "parser", 1200),
  item("Document parser vision projector", "parser", 400),
]);

const idle: DownloadState = { plan: base, running: false, groups: null, got: {}, interrupted: null };

test("rowAction: offers the download of a missing part with its size", () => {
  assert.deepEqual(rowAction("embedder", false, idle), { kind: "download", bytes: 800 });
  assert.deepEqual(rowAction("parser", false, idle), { kind: "download", bytes: 1600 });
});

test("rowAction: installed wins, from the status page or when the plan has nothing missing", () => {
  assert.deepEqual(rowAction("embedder", true, idle), { kind: "installed" });
  assert.deepEqual(rowAction("pdfium", false, idle), { kind: "installed" });
});

test("rowAction: no size when the manifest has none (llama.cpp zips)", () => {
  const p = plan([item("llama.cpp", "llama", null), item("CUDA runtime libraries", "llama", null)]);
  assert.deepEqual(rowAction("llama", false, { ...idle, plan: p }), { kind: "download", bytes: null });
  const r = rowAction("llama", false, { ...idle, plan: p, running: true, groups: ["llama"], got: { "llama.cpp": 70 } });
  assert.deepEqual(r, { kind: "downloading", got: 70, total: 70, frac: null });
});

test("rowAction: progress only on the rows being downloaded; the others wait", () => {
  const s: DownloadState = { ...idle, running: true, groups: ["parser"], got: { "Document parser model": 600 } };
  assert.deepEqual(rowAction("parser", false, s), { kind: "downloading", got: 600, total: 1600, frac: 600 / 1600 });
  assert.deepEqual(rowAction("embedder", false, s), { kind: "waiting" });
  assert.deepEqual(rowAction("pdfium", false, s), { kind: "installed" });
});

test("rowAction: an automatic first-run download (groups unknown) counts as the default set", () => {
  const s: DownloadState = { ...idle, running: true, got: { "EmbeddingGemma-2 model": 300, "EmbeddingGemma-2 vision projector": 100 } };
  assert.deepEqual(rowAction("embedder", false, s), { kind: "downloading", got: 400, total: 800, frac: 0.5 });
  assert.deepEqual(rowAction("parser", false, s), { kind: "waiting" });
});

test("rowAction: a failed download offers a retry on its own rows only", () => {
  const s: DownloadState = { ...idle, groups: ["embedder"], interrupted: "stalled: no data for 20s" };
  assert.deepEqual(rowAction("embedder", false, s), { kind: "failed", error: "stalled: no data for 20s", bytes: 800 });
  assert.deepEqual(rowAction("parser", false, s), { kind: "download", bytes: 1600 });
  // the automatic download failed: the default rows offer the retry
  assert.equal(rowAction("embedder", false, { ...s, groups: null }).kind, "failed");
});
