// Browser-preview backend (used when not running inside Tauri): mirrors the Rust
// MockSource shapes so `npm run dev` in a normal browser shows a working popup.
import type { Backend, Filter, Hit, ModelsDone, ModelsPlan, ModelsProgress, PausedReason, ServiceState, Sort, Tier1Status, VolumeStatus } from "./types";

const WORDS = ["invoice","report","budget","holiday","photo","screenshot","meeting","notes","draft","final","contract","resume","backup","project","design","summer","winter","family","tax","receipt","presentation","analysis","readme","config","archive","setup","installer","demo","logo","icon","paris","tokyo","berlin","wedding","birthday","lecture","thesis","model","dataset","export"];
const EXTS: [string, number][] = [["jpg",1],["png",1],["heic",1],["pdf",2],["docx",2],["xlsx",2],["txt",2],["md",2],["pptx",2],["mp4",3],["mkv",3],["mp3",4],["flac",4],["wav",4],["zip",0],["exe",0],["rs",0],["py",0],["json",0],["log",0]];
const ROOTS = ["C:\\Users\\User\\Documents","C:\\Users\\User\\Pictures","C:\\Users\\User\\Downloads","C:\\Users\\User\\Desktop","D:\\Projects\\winseek\\crates\\index\\src","D:\\Media\\Videos","D:\\Media\\Music","D:\\Archive\\2019\\Old Laptop Backup\\Documents"];
const SUBS = ["2024","2025","Work","Personal","Misc","Clients","Scans","Camera Roll"];

interface E { name: string; lower: string; path: string; dir: boolean; size: number; mtime: number; kind: number }

let seed = 0x9e3779b9;
const rnd = () => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) / 4294967296; };
const pick = <T,>(a: T[]): T => a[Math.floor(rnd() * a.length)];

const DB: E[] = (() => {
  const now = Date.now();
  const out: E[] = [];
  for (let i = 0; i < 30000; i++) {
    const dir = `${pick(ROOTS)}\\${pick(SUBS)}`;
    if (i % 40 === 0) {
      const name = `${pick(WORDS)} ${pick(SUBS)}`;
      out.push({ name, lower: name.toLowerCase(), path: `${dir}\\${name}`, dir: true, size: 0, mtime: now - rnd() * 3e10, kind: 5 });
      continue;
    }
    const [ext, kind] = pick(EXTS);
    const name = kind === 1 ? `IMG_${String(Math.floor(rnd() * 9999)).padStart(4, "0")}.${ext}` : `${pick(WORDS)}_${pick(WORDS)}_${Math.floor(rnd() * 100)}.${ext}`;
    const size = Math.floor(rnd() * 4e6 * (kind === 3 ? 300 : 1)) + 200;
    out.push({ name, lower: name.toLowerCase(), path: `${dir}\\${name}`, dir: false, size, mtime: now - Math.pow(rnd(), 3) * 3e10, kind });
  }
  return out;
})();

// ---- name-service simulation (query: ?installed=1 skips first run, ?stopped=1, ?fail=1, ?accent=1) ----
const params = new URLSearchParams(typeof location !== "undefined" ? location.search : "");
const SCAN_MS = 4000;
const VOLS = [
  { letter: "C", files: 5210442, folders: 740569, ms: SCAN_MS },
  { letter: "H", files: 998120, folders: 206391, ms: SCAN_MS * 0.6 },
];
let svc: ServiceState = params.get("installed") === "1" ? "running" : params.get("stopped") === "1" ? "stopped" : "not_installed";
let scanStart: number | null = svc === "running" ? -Infinity : null; // when the "pipe" comes up
const listeners = new Set<() => void>();
function bringUp() {
  svc = "running";
  scanStart = performance.now() + 300; // pipe connects shortly after the service starts
  setTimeout(() => listeners.forEach((f) => f()), 300);
}

const bin: { e: E; at: number }[] = [];

const toHit = (e: E): Hit => ({ name: e.name, path: e.path, is_dir: e.dir, size: e.dir ? null : e.size, mtime: Math.floor(e.mtime) });
const okFilter = (e: E, f: Filter) => f === "all" || (f === "images" && e.kind === 1) || (f === "docs" && e.kind === 2) || (f === "video" && e.kind === 3) || (f === "audio" && e.kind === 4) || (f === "folders" && e.dir);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---- search-by-meaning simulation ----
// query: ?models=1 (models already installed, indexing runs), ?dlfail=1 (first download attempt fails at ~60%),
// ?unavailable=1 (Tier 1 store failed to open), ?battery=1 / ?fullscreen=1 (indexing auto-paused),
// ?gpu=vulkan|cpu (default: NVIDIA / CUDA)
const T1_FILES = 3880;
const T1_RATE = 120; // files per second once indexing runs
const gpuParam = params.get("gpu");
const MOCK_BACKEND: ModelsPlan["backend"] = gpuParam === "cpu" ? "cpu" : gpuParam === "vulkan" ? "vulkan" : "cuda12";
const MOCK_ITEMS: { name: string; bytes: number | null; real: number }[] = [
  { name: `llama.cpp b11588 (${MOCK_BACKEND === "cuda12" ? "CUDA 12" : MOCK_BACKEND === "vulkan" ? "Vulkan" : "CPU"})`, bytes: null, real: MOCK_BACKEND === "cuda12" ? 210e6 : 40e6 },
  ...(MOCK_BACKEND === "cuda12" ? [{ name: "CUDA runtime libraries", bytes: null, real: 370e6 }] : []),
  { name: "EmbeddingGemma-2 model", bytes: 309855456, real: 309855456 },
  { name: "EmbeddingGemma-2 vision projector", bytes: 554821024, real: 554821024 },
];
let modelsInstalled = params.get("models") === "1";
let dlRunning = false;
let dlCancel = false;
let dlFailOnce = params.get("dlfail") === "1";
const dlDone: Record<string, number> = {}; // bytes kept per item (partial files survive cancel / failure)
const progressCbs = new Set<(p: ModelsProgress) => void>();
const doneCbs = new Set<(d: ModelsDone) => void>();
let include = ["C:\\Users\\User\\Pictures", "C:\\Users\\User\\Documents", "C:\\Users\\User\\Desktop", "C:\\Users\\User\\Downloads"];
let exclude: string[] = [];
let userPaused = false;
let indexed = 0;
let indexTick = performance.now();

const itemHave = (i: { name: string; real: number }) => (dlDone[i.name] ?? 0) >= i.real;

async function runDownload() {
  dlRunning = true;
  dlCancel = false;
  const all = MOCK_ITEMS.reduce((a, i) => a + i.real, 0);
  const fail = (error: string) => { dlRunning = false; doneCbs.forEach((f) => f({ ok: false, error })); };
  for (const item of MOCK_ITEMS) {
    while ((dlDone[item.name] ?? 0) < item.real) {
      await sleep(100);
      if (dlCancel) return fail("cancelled");
      dlDone[item.name] = Math.min(item.real, (dlDone[item.name] ?? 0) + 30e6);
      const got = MOCK_ITEMS.reduce((a, i) => a + (dlDone[i.name] ?? 0), 0);
      progressCbs.forEach((f) => f({ item: item.name, downloaded: dlDone[item.name], total: item.real, overall_downloaded: got, overall_total: all }));
      if (dlFailOnce && got / all > 0.6) { dlFailOnce = false; return fail("stalled: no data for 20s at 612345678 bytes"); }
    }
  }
  modelsInstalled = true;
  indexTick = performance.now();
  dlRunning = false;
  doneCbs.forEach((f) => f({ ok: true, error: null }));
}

function t1Status(): Tier1Status {
  if (params.get("unavailable") === "1") {
    return { state: "unavailable", stats: null, include: [], exclude: [], error: "open store: database is locked", paused_reason: null };
  }
  const reason: PausedReason | null = userPaused ? "user" : params.get("battery") === "1" ? "battery" : params.get("fullscreen") === "1" ? "fullscreen" : null;
  const now = performance.now();
  if (modelsInstalled && !reason) indexed = Math.min(T1_FILES, indexed + ((now - indexTick) / 1000) * T1_RATE);
  indexTick = now;
  const done = Math.floor(indexed);
  const state = !modelsInstalled ? "models_missing" : reason ? "paused" : done < T1_FILES ? "indexing" : "idle";
  return {
    state,
    stats: { files: T1_FILES, pending: T1_FILES - done, done, failed: modelsInstalled && done > 100 ? 3 : 0, skipped: 0, vectors: done },
    include: [...include], exclude: [...exclude], error: null, paused_reason: reason,
  };
}

const hash01 = (s: string) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return ((h >>> 0) % 10000) / 10000;
};
const inScope = (path: string) => include.some((r) => path.toLowerCase().startsWith(r.toLowerCase() + "\\"));

export const mockBackend: Backend = {
  async search({ query, offset, limit, sort, filter }) {
    const t0 = performance.now();
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    const m = DB.filter((e) => okFilter(e, filter) && terms.every((t) => e.lower.includes(t)));
    const s: Sort = sort === "rel" && terms.length === 0 ? "date" : sort;
    if (s === "name") m.sort((a, b) => a.lower.localeCompare(b.lower));
    else if (s === "size") m.sort((a, b) => b.size - a.size);
    else if (s === "date") m.sort((a, b) => b.mtime - a.mtime);
    else m.sort((a, b) => Number(b.lower.startsWith(terms[0])) - Number(a.lower.startsWith(terms[0])) || a.lower.length - b.lower.length);
    await sleep(8);
    return { total: m.length, hits: m.slice(offset, offset + limit).map(toHit), elapsed_us: Math.round((performance.now() - t0) * 1000) };
  },
  async status(): Promise<VolumeStatus[]> {
    if (scanStart === null || performance.now() < scanStart) return [{ letter: "C", files: 0, folders: 0, state: "demo" }];
    const el = performance.now() - scanStart;
    return VOLS.map((v) => {
      const total = v.files + v.folders;
      if (el >= v.ms) return { letter: v.letter, files: v.files, folders: v.folders, state: "live", done: null, total: null };
      const p = 1 - Math.pow(1 - el / v.ms, 2);
      return { letter: v.letter, files: 0, folders: 0, state: "scanning", done: Math.floor(total * p), total };
    });
  },
  async recent(limit) { return [...DB].filter((e) => !e.dir).sort((a, b) => b.mtime - a.mtime).slice(0, limit).map(toHit); },
  async openPath(p) { console.info("[mock] open", p); },
  async revealPath(p) { console.info("[mock] reveal", p); },
  async copyPath(p) { try { await navigator.clipboard.writeText(p); } catch { /* ignore */ } console.info("[mock] copy", p); },
  async copyFiles(p) { console.info("[mock] copy files", p); },
  async recycle(paths) {
    for (const path of paths) {
      const i = DB.findIndex((e) => e.path === path);
      if (i < 0) throw new Error("File not found");
      bin.push({ e: DB.splice(i, 1)[0], at: Date.now() });
    }
    console.info("[mock] recycle", paths);
  },
  async restoreRecycled(paths) {
    let n = 0;
    for (const path of paths) {
      let k = -1;
      bin.forEach((b, i) => { if (b.e.path === path && (k < 0 || b.at >= bin[k].at)) k = i; });
      if (k >= 0) { DB.push(bin.splice(k, 1)[0].e); n++; }
    }
    console.info("[mock] restore", paths, n);
    return n;
  },
  async renamePath(path, newName) {
    if (!newName.trim() || /[\\/:*?"<>|]/.test(newName)) throw new Error("Invalid file name");
    const e = DB.find((x) => x.path === path);
    if (!e) throw new Error("File not found");
    const np = path.slice(0, path.lastIndexOf("\\") + 1) + newName;
    if (DB.some((x) => x.path === np)) throw new Error("A file with that name already exists");
    e.name = newName; e.lower = newName.toLowerCase(); e.path = np;
    console.info("[mock] rename", path, "->", np);
    return np;
  },
  async showContextMenu(path, x, y) { console.info("[mock] context menu", path, x, y); return "cancelled"; },
  async startDrag(paths) { console.info("[mock] start drag", paths); return "none"; },
  iconUrl() { return null; },
  async hide() { console.info("[mock] hide"); },
  async onShown() { return () => {}; },
  async serviceState() { return svc; },
  async installService() {
    await sleep(1500);
    if (params.get("fail") === "1") throw new Error("Installation cancelled");
    bringUp();
  },
  async startService() { await sleep(500); bringUp(); },
  async accentPalette() {
    return params.get("accent") === "1"
      ? ["#d6c6f5", "#b9a0ee", "#9a78e2", "#7b52d3", "#6038b8", "#4a2a93", "#351d6b", "#201040"]
      : null;
  },
  async tier1Status() { return t1Status(); },
  async tier1SetScopes(inc, exc) { await sleep(30); include = [...inc]; exclude = [...exc]; },
  async tier1Pause(p) { userPaused = p; },
  async semanticSearch(query, k) {
    const st = t1Status().state;
    if (!modelsInstalled || st === "unavailable") return { state: st === "unavailable" ? "unavailable" : "models_missing", hits: [] };
    await sleep(60 + Math.random() * 60);
    const q = query.toLowerCase().trim();
    const terms = q.split(/\s+/).filter(Boolean);
    if (!q) return { state: st, hits: [] };
    // Fake "image embeddings": stable pseudo-scores per (query, file); files whose name also matches a term
    // rank high, so the "name + meaning" chip and the dedupe are visible in the preview.
    const hits = DB.filter((e) => e.kind === 1 && inScope(e.path))
      .map((e) => ({ path: e.path, score: 0.1 + 0.22 * hash01(q + "|" + e.path) + (terms.some((t) => e.lower.includes(t)) ? 0.2 : 0) }))
      .sort((a, b) => b.score - a.score)
      .slice(0, Math.max(1, Math.min(k, 200)));
    return { state: st, hits };
  },
  async modelsPlan(): Promise<ModelsPlan> {
    await sleep(MOCK_BACKEND === "cpu" ? 100 : 600); // GPU detection takes a moment
    const items = MOCK_ITEMS.map((i) => ({ name: i.name, bytes: i.bytes, present: modelsInstalled || itemHave(i) }));
    return {
      backend: MOCK_BACKEND,
      gpu: MOCK_BACKEND === "cuda12" ? "NVIDIA GeForce RTX 4070" : MOCK_BACKEND === "vulkan" ? "AMD Radeon RX 6600" : null,
      driver: MOCK_BACKEND === "cuda12" ? "560.94" : null,
      items,
      total_bytes_missing: items.filter((i) => !i.present).reduce((a, i) => a + (i.bytes ?? 0), 0),
      size_unknown: items.some((i) => !i.present && i.bytes == null),
      downloading: dlRunning,
    };
  },
  async modelsDownload() {
    if (dlRunning) throw new Error("A download is already running");
    void runDownload();
  },
  async modelsCancel() { dlCancel = true; },
  async onModelsProgress(cb) { progressCbs.add(cb); return () => { progressCbs.delete(cb); }; },
  async onModelsDone(cb) { doneCbs.add(cb); return () => { doneCbs.delete(cb); }; },
  async onSourceChanged(cb) { listeners.add(cb); return () => { listeners.delete(cb); }; },
};
