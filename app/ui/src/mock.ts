// Browser-preview backend (used when not running inside Tauri): mirrors the Rust
// MockSource shapes so `npm run dev` in a normal browser shows a working popup.
import { buildLibrary, tileUrl, type LibFile } from "./mockLibrary";
import type { Backend, Collection, DupGroup, Filter, Hit, LibImage, NearState, ModelsDone, ModelsPlan, ModelsProgress, PausedReason, ParsedDoc, PreviewInfo, Profile, SemanticHit, ServiceState, Sort, StatusPage, Tier1Status, VolumeDetail, VolumeStatus } from "./types";

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

// ---- library window data (images over three years, duplicates, big files) ----
const LIB = buildLibrary(Date.now());
const libByPath = new Map<string, LibFile>();
for (const f of LIB) {
  libByPath.set(f.path, f);
  DB.push({ name: f.name, lower: f.name.toLowerCase(), path: f.path, dir: false, size: f.size, mtime: f.mtime, kind: f.kind });
}
const livePaths = () => new Set(DB.map((e) => e.path));
let collections: Collection[] = [
  { name: "Receipts", query: "a photo of a paper receipt or invoice", threshold: 0.64 },
  { name: "Whiteboards", query: "a whiteboard with handwritten notes and diagrams", threshold: 0.61 },
  { name: "Memes", query: "a meme: an image with a funny caption", threshold: 0.63 },
  { name: "Documents to sign", query: "a contract or form with a signature line to sign", threshold: 0.63 },
  { name: "Screenshots", query: "", threshold: 0, rule: "screenshots" },
];
let nearStartedAt: number | null = null;
const NEAR_MS = 2400;

// ---- name-service simulation (query: ?installed=1 skips first run, ?stopped=1, ?fail=1, ?accent=1,
// ?searchd=down: running service that does not answer, ?volerr=1: volume H reports an error) ----
const params = new URLSearchParams(typeof location !== "undefined" ? location.search : "");
const SCAN_MS = 4000;
const VOLS: { letter: string; files: number; folders: number; ms: number; fs: string; monitor: VolumeDetail["monitor"] }[] = [
  { letter: "C", files: 5210442, folders: 740569, ms: SCAN_MS, fs: "NTFS", monitor: "usn" },
  { letter: "H", files: 998120, folders: 206391, ms: SCAN_MS * 0.6, fs: "NTFS", monitor: "usn" },
  { letter: "E", files: 18342, folders: 1207, ms: SCAN_MS * 0.3, fs: "exFAT", monitor: "folder_watch" },
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
const okFilter = (e: E, f: Filter) => f === "all" || (f === "images" && e.kind === 1) || (f === "docs" && e.kind === 2) || (f === "video" && e.kind === 3) || (f === "audio" && e.kind === 4) || (f === "folders" && e.dir) || (f === "apps" && /\.(exe|msi)$/.test(e.lower));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---- search-by-meaning simulation ----
// query: ?models=1 (models already installed, indexing runs), ?dlfail=1 (first download attempt fails at ~60%),
// ?unavailable=1 (Tier 1 store failed to open), ?battery=1 / ?fullscreen=1 (indexing auto-paused),
// ?gpu=vulkan|cpu (default: NVIDIA / CUDA)
const T1_FILES = 3880;
const T1_RATE = 120; // files per second once indexing runs
const TEXT_ONLY = 412; // documents + PDFs already searchable by text while the models are missing
const gpuParam = params.get("gpu");
const MOCK_BACKEND: ModelsPlan["backend"] = gpuParam === "cpu" ? "cpu" : gpuParam === "vulkan" ? "vulkan" : "cuda12";
const MOCK_ITEMS: { name: string; bytes: number | null; real: number }[] = [
  { name: `llama.cpp b11588 (${MOCK_BACKEND === "cuda12" ? "CUDA 12" : MOCK_BACKEND === "vulkan" ? "Vulkan" : "CPU"})`, bytes: null, real: MOCK_BACKEND === "cuda12" ? 210e6 : 40e6 },
  ...(MOCK_BACKEND === "cuda12" ? [{ name: "CUDA runtime libraries", bytes: null, real: 370e6 }] : []),
  { name: "EmbeddingGemma-2 model", bytes: 309855456, real: 309855456 },
  { name: "EmbeddingGemma-2 vision projector", bytes: 554821024, real: 554821024 },
  { name: "PDF engine (pdfium 7881)", bytes: 3733154, real: 3733154 },
];
// Optional document parser (Infinity-Parser2). ?parser=0: not installed, so the preview shows the disabled item
// and Setup offers the download.
const PARSER_ITEMS: { name: string; bytes: number | null; real: number }[] = [
  { name: "Document parser model", bytes: 1274396736, real: 1274396736 },
  { name: "Document parser vision projector", bytes: 336000000, real: 336000000 },
];
let parserInstalled = params.get("parser") !== "0";
let lastParseAt: number | null = null; // when the parser server was last used (it unloads after IDLE_UNLOAD_S)
let mockProfile: Profile = "balanced";
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
// A representative slice of winseek_store::DEFAULT_IGNORE.
const IGNORE_DEFAULTS = [
  "AppData", "$RECYCLE.BIN", "Thumbs.db", ".ssh", ".gnupg", "*.kdbx", ".git", ".hg", ".svn", ".idea", ".vs", ".vscode",
  "node_modules", ".next", ".yarn", ".pnpm-store", "venv", ".venv", "__pycache__", "*.egg-info", "site-packages",
  ".pytest_cache", ".mypy_cache", "target", ".cargo", ".gradle", ".m2", ".mvn", "build", "dist", "out", "bin", "obj",
  "cmake-build-*", "bazel-*", "vendor", "Pods", "DerivedData", ".dart_tool", ".terraform",
];
let ignoreList: string[] | null = null;
let indexCode = false;
let indexed = 0;
let indexTick = performance.now();

const itemHave = (i: { name: string; real: number }) => (dlDone[i.name] ?? 0) >= i.real;
const parserHave = () => parserInstalled || PARSER_ITEMS.every(itemHave);
// ?pdfium=0 shows the "PDF reader is being downloaded" state; otherwise the engine is there.
const pdfiumHave = () => params.get("pdfium") !== "0" || modelsInstalled || itemHave(MOCK_ITEMS[MOCK_ITEMS.length - 1]);

async function runDownload(withParser: boolean) {
  dlRunning = true;
  dlCancel = false;
  // Only what is missing is downloaded: the base items unless already installed, the parser only when asked for.
  const items = [...(modelsInstalled ? [] : MOCK_ITEMS), ...(withParser && !parserHave() ? PARSER_ITEMS : [])];
  const all = items.reduce((a, i) => a + i.real, 0);
  const fail = (error: string) => { dlRunning = false; doneCbs.forEach((f) => f({ ok: false, error })); };
  for (const item of items) {
    while ((dlDone[item.name] ?? 0) < item.real) {
      await sleep(100);
      if (dlCancel) return fail("cancelled");
      dlDone[item.name] = Math.min(item.real, (dlDone[item.name] ?? 0) + 30e6);
      const got = items.reduce((a, i) => a + (dlDone[i.name] ?? 0), 0);
      progressCbs.forEach((f) => f({ item: item.name, downloaded: dlDone[item.name], total: item.real, overall_downloaded: got, overall_total: all }));
      if (dlFailOnce && got / all > 0.6) { dlFailOnce = false; return fail("stalled: no data for 20s at 612345678 bytes"); }
    }
  }
  modelsInstalled = true;
  if (withParser) parserInstalled = true;
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
    // Before the models exist the documents / PDFs are still read: their text is searchable ("text only").
    stats: {
      files: T1_FILES, pending: T1_FILES - done - (modelsInstalled ? 0 : TEXT_ONLY), done, failed: modelsInstalled && done > 100 ? 3 : 0,
      skipped: 0, vectors: done, text_only: modelsInstalled ? 0 : TEXT_ONLY,
    },
    include: [...include], exclude: [...exclude], error: null, paused_reason: reason,
  };
}

// ---- status page ----
const SERVER_PORT = 51234;
const IDLE_UNLOAD_S = 120;
const rescanAt: Record<string, number> = {}; // letter -> performance.now() of the click
let lastEmbedAt: number | null = null; // last time the (fake) embedding server was busy
const bootAt = performance.now();
let rateSamples: { t: number; done: number }[] = [];

function mockStatusPage(): StatusPage {
  const now = performance.now();
  const t1 = t1Status();
  const down = params.get("searchd") === "down" && svc === "running";
  const up = svc === "running" && scanStart !== null && now >= scanStart;
  const source = down ? "unavailable" : up ? "searchd" : "demo";
  const volumes: VolumeDetail[] = down ? [] : !up
    ? [{ letter: "C", files: 12580243, folders: 1204511, state: "demo", done: null, total: null, error: null, fs: "", monitor: "" }]
    : VOLS.map((v): VolumeDetail => ({ ...volRow(v), fs: v.fs, monitor: v.monitor }));
  function volRow(v: (typeof VOLS)[number]): Omit<VolumeDetail, "fs" | "monitor"> {
    const total = v.files + v.folders;
    const el = now - (scanStart as number);
    const rs = rescanAt[v.letter] !== undefined ? now - rescanAt[v.letter] : Infinity;
    if (v.letter === "H" && params.get("volerr") === "1") {
      return { letter: "H", files: 0, folders: 0, state: "error", done: null, total: null, error: "USN journal is not active and could not be created (access denied)" };
    }
    if (rs < 1500) return { letter: v.letter, files: v.files, folders: v.folders, state: "rescanning", done: null, total: null, error: null };
    if (rs < 1500 + v.ms) {
      const p = 1 - Math.pow(1 - (rs - 1500) / v.ms, 2);
      return { letter: v.letter, files: 0, folders: 0, state: "scanning", done: Math.floor(total * p), total, error: null };
    }
    if (el >= v.ms) return { letter: v.letter, files: v.files, folders: v.folders, state: "live", done: null, total: null, error: null };
    const p = 1 - Math.pow(1 - el / v.ms, 2);
    return { letter: v.letter, files: 0, folders: 0, state: "scanning", done: Math.floor(total * p), total, error: null };
  }

  const s = t1.stats;
  const indexing = t1.state === "indexing";
  if (indexing) lastEmbedAt = now;
  // Rolling rate over the last ~60 s of samples, like the real worker.
  if (s) {
    rateSamples.push({ t: now, done: s.done });
    rateSamples = rateSamples.filter((x) => now - x.t <= 60000);
  }
  const first = rateSamples[0];
  const rate = indexing && s && first && now - first.t >= 1000 ? Math.max(0, (s.done - first.done) / ((now - first.t) / 1000)) || null : null;
  const queue = s ? s.pending + s.text_only : 0;
  const failed = s?.failed ?? 0;
  const denom = s ? s.done + s.pending + s.text_only + failed : 0;
  const idleFor = lastEmbedAt === null ? Infinity : (now - lastEmbedAt) / 1000;
  const running = modelsInstalled && t1.state !== "unavailable" && idleFor < IDLE_UNLOAD_S;
  const bytes = (expected: number, name: string) => (modelsInstalled || (dlDone[name] ?? 0) >= expected ? expected : null);
  return {
    tier0: {
      source,
      service: svc,
      uptime_s: source === "searchd" ? Math.floor((now - bootAt) / 1000) + 5400 : null,
      volumes,
      can_rescan: source === "searchd",
      rescan_hint: source === "searchd" ? null : source === "demo" ? "Needs the name service, which is not running." : "The name service is not answering.",
    },
    tier1: {
      state: t1.state,
      paused_reason: t1.paused_reason,
      error: t1.error,
      stats: t1.stats,
      coverage: denom > 0 && s ? s.done / denom : null,
      queue,
      rate: t1.state === "indexing" ? rate : null,
      eta_s: rate && queue > 0 ? Math.ceil(queue / rate) : null,
      can_rebuild: t1.state !== "unavailable",
      ocr_unavailable: params.get("noocr") === "1",
    },
    models: {
      llama_build: "b11588",
      backend: MOCK_BACKEND,
      gpu: MOCK_BACKEND === "cuda12" ? "NVIDIA GeForce RTX 4070" : MOCK_BACKEND === "vulkan" ? "AMD Radeon RX 6600" : null,
      llama_present: modelsInstalled || MOCK_ITEMS.filter((i) => i.name.startsWith("llama.cpp")).every(itemHave),
      model_id: "embeddinggemma-2",
      profile: "default",
      quant: "Q8_0",
      files: [
        { name: "embeddinggemma-2-Q8_0.gguf", role: "model", expected_bytes: 309855456, bytes: bytes(309855456, "EmbeddingGemma-2 model") },
        { name: "mmproj-embeddinggemma-2-Q8_0.gguf", role: "mmproj", expected_bytes: 554821024, bytes: bytes(554821024, "EmbeddingGemma-2 vision projector") },
      ],
      pdfium: pdfiumHave()
        ? { build: "7881", present: true, source: "installed", expected_bytes: 3733154, bytes: 8_104_960 }
        : { build: "7881", present: false, source: "missing", expected_bytes: 3733154, bytes: null },
      server: {
        running,
        port: running ? SERVER_PORT : null,
        unload_in_s: running ? Math.max(0, Math.round(IDLE_UNLOAD_S - idleFor)) : null,
        idle_unload_s: IDLE_UNLOAD_S,
      },
      parser: {
        model_id: "infinity-parser2-flash",
        present: parserHave(),
        files: [
          { name: "Infinity-Parser2-Flash.Q4_K_M.gguf", role: "model", expected_bytes: 1274396736, bytes: parserHave() ? 1274396736 : null },
          { name: "Infinity-Parser2-Flash.mmproj-q8_0.gguf", role: "mmproj", expected_bytes: 336000000, bytes: parserHave() ? 336000000 : null },
        ],
        server: parserServer(now),
        queue: parserHave() ? { queued: modelsInstalled ? 3 : 0, done: modelsInstalled ? 41 : 0, failed: 0 } : { queued: 0, done: 0, failed: 0 },
        profile: mockProfile,
        routing: { doc_min: 0.6, doc_margin: 0.04, screenshot_min_chars: 200, provisional: false },
      },
    },
  };
}

function parserServer(now: number): StatusPage["models"]["parser"]["server"] {
  const idle = lastParseAt === null ? Infinity : (now - lastParseAt) / 1000;
  const running = idle < IDLE_UNLOAD_S;
  return { running, port: running ? SERVER_PORT + 1 : null, unload_in_s: running && mockProfile !== "fast" ? Math.max(0, Math.round(IDLE_UNLOAD_S - idle)) : null, idle_unload_s: IDLE_UNLOAD_S };
}

// What the parser "reads" from any image or page: an invoice with a table (a second table on even PDF pages).
const MOCK_TABLE_A = {
  html: "<table><tr><th>Item</th><th>Qty</th><th>Price</th></tr><tr><td>Graphics card</td><td>1</td><td>1,249.00</td></tr><tr><td>NVMe drive 2TB</td><td>2</td><td>310.00</td></tr><tr><td>Total due</td><td></td><td>1,869.00</td></tr></table>",
  tsv: "Item\tQty\tPrice\nGraphics card\t1\t1,249.00\nNVMe drive 2TB\t2\t310.00\nTotal due\t\t1,869.00",
};
const MOCK_TABLE_B = {
  html: "<table><tr><th>Month</th><th>Total</th></tr><tr><td>March</td><td>1,869.00</td></tr></table>",
  tsv: "Month\tTotal\nMarch\t1,869.00",
};
const MOCK_MARKDOWN = [
  "# Quarterly Invoice 4471",
  "Billed by Winseek Hardware Ltd, 12 Rue des Fleurs, Casablanca.",
  "| Item | Qty | Price |\n| --- | --- | --- |\n| Graphics card | 1 | 1,249.00 |\n| NVMe drive 2TB | 2 | 310.00 |\n| Total due |  | 1,869.00 |",
  "Page 1 of 1",
].join("\n\n");
const parsedCache = new Set<string>();

const hash01 = (s: string) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return ((h >>> 0) % 10000) / 10000;
};
const inScope = (path: string) => include.some((r) => path.toLowerCase().startsWith(r.toLowerCase() + "\\"));


// ---- Space preview: small generated samples (no files on disk in a plain browser) ----
const PV_IMG = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "heic"];
const PV_VIDEO = ["mp4", "mkv", "mov", "avi", "webm"];
const PV_AUDIO = ["mp3", "flac", "wav", "m4a", "ogg"];
const PV_TEXT = ["txt", "md", "log", "json", "rs", "py", "ts", "toml", "csv"];
const PV_PAGES = (path: string) => 3 + Math.floor(hash01(path + "pages") * 14); // 3..16 pages
// Words per page that the mock "text layer" contains, so a semantic / text hit can name the page it is on.
const PDF_LINES = [
  "Invoice no. 2025-0142 total due 240.00 EUR",
  "Meeting notes: budget review and next steps",
  "Receipt Paris cafe 12.50 EUR thank you",
  "Quarterly report: revenue grew and costs fell",
];
const pdfPage = (path: string, line: number) => 1 + Math.floor(hash01(path + "|" + line) * (PV_PAGES(path) - 1));

function svgPdfPage(name: string, page: number, pages: number, width: number): string {
  const w = 595, h = 842;
  const lines = PDF_LINES.map((t, i) => `<text x="56" y="${150 + i * 34}" font-family="Segoe UI, sans-serif" font-size="15" fill="#333">${pdfPage(name, i) === page ? "&#9733; " : ""}${t}</text>`).join("");
  const bars = Array.from({ length: 14 }, (_, i) => `<rect x="56" y="${320 + i * 28}" width="${300 + Math.floor(hash01(name + page + i) * 180)}" height="8" rx="4" fill="#d9dce3"/>`).join("");
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${Math.round(width * h / w)}" viewBox="0 0 ${w} ${h}">` +
    `<rect width="${w}" height="${h}" fill="#fff"/><rect x="28" y="28" width="${w - 56}" height="${h - 56}" fill="none" stroke="#e5e7ee"/>` +
    `<text x="56" y="86" font-family="Segoe UI, sans-serif" font-size="26" font-weight="700" fill="#1d2433">${name.replace(/[<>&]/g, "")}</text>` +
    `<text x="56" y="112" font-family="Segoe UI, sans-serif" font-size="13" fill="#7b8190">Page ${page} of ${pages}</text>${lines}${bars}</svg>`;
  return svg;
}
const pvExt = (n: string) => n.slice(n.lastIndexOf(".") + 1).toLowerCase();

function svgImage(name: string, w: number, h: number): string {
  const hue = Math.floor(hash01(name) * 360);
  const cells: string[] = [];
  for (let x = 0; x < w; x += 100) cells.push(`<path d="M${x} 0V${h}" />`);
  for (let y = 0; y < h; y += 100) cells.push(`<path d="M0 ${y}H${w}" />`);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue} 70% 62%)"/><stop offset="1" stop-color="hsl(${(hue + 70) % 360} 65% 38%)"/></linearGradient></defs>` +
    `<rect width="${w}" height="${h}" fill="url(#g)"/><g stroke="#fff" stroke-opacity=".25" stroke-width="2">${cells.join("")}</g>` +
    `<circle cx="${w / 2}" cy="${h / 2}" r="${h / 4}" fill="none" stroke="#fff" stroke-width="6"/>` +
    `<text x="${w / 2}" y="${h / 2 + 22}" font-family="Segoe UI, sans-serif" font-size="64" font-weight="700" fill="#fff" text-anchor="middle">${name.replace(/[<>&]/g, "")}</text>` +
    `<text x="24" y="${h - 24}" font-family="Segoe UI, sans-serif" font-size="36" fill="#fff" fill-opacity=".8">${w} x ${h}</text></svg>`;
  return "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
}

function sampleText(name: string, ext: string): string {
  const words = name.replace(/\.[^.]+$/, "").split(/[_\s-]+/).filter((w) => w && !/^\d+$/.test(w));
  const topic = words.join(" ") || "notes";
  const L: string[] = [];
  if (ext === "rs" || ext === "py" || ext === "ts") {
    const id = words.join("_").toLowerCase() || "item";
    L.push("// sample file generated by the browser mock");
    for (let i = 0; i < 60; i++) L.push(i % 7 === 0 ? `fn helper_${i}(x: u32) -> u32 {` : i % 7 === 6 ? "}" : `    let v${i} = x * ${i} + 1;`);
    L.push("", `/// ${topic}: the part a search for "${words[0] ?? "notes"}" lands on.`, `pub fn ${id}(input: &str) -> usize {`, `    input.split_whitespace().filter(|w| w.contains("${words[0] ?? "x"}")).count()`, "}");
    for (let i = 0; i < 40; i++) L.push(`// trailing line ${i}`);
  } else {
    L.push("Sample document", "===============", "");
    for (let i = 0; i < 70; i++) L.push(i % 9 === 8 ? "" : `Line ${i + 1}: the quick brown fox jumps over the lazy dog, again and again until the page is full.`);
    L.push("", `Summary of ${topic}`, `The ${topic} section is where the ${words[0] ?? "main"} figures are discussed in detail.`, `Totals for ${topic} are carried over to next quarter.`, "");
    for (let i = 0; i < 40; i++) L.push(`Appendix ${i + 1}: more filler text so that the first match is not at the top.`);
  }
  return L.join(String.fromCharCode(10));
}

function wavUrl(): string {
  const rate = 8000, n = rate * 2, buf = new ArrayBuffer(44 + n * 2), v = new DataView(buf);
  const w = (o: number, t: string) => { for (let i = 0; i < t.length; i++) v.setUint8(o + i, t.charCodeAt(i)); };
  w(0, "RIFF"); v.setUint32(4, 36 + n * 2, true); w(8, "WAVE"); w(12, "fmt "); v.setUint32(16, 16, true); v.setUint16(20, 1, true);
  v.setUint16(22, 1, true); v.setUint32(24, rate, true); v.setUint32(28, rate * 2, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
  w(36, "data"); v.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i++) v.setInt16(44 + i * 2, Math.sin((i / rate) * 2 * Math.PI * (i < rate ? 440 : 660)) * 6000, true);
  let bin = "";
  new Uint8Array(buf).forEach((b) => { bin += String.fromCharCode(b); });
  return "data:audio/wav;base64," + btoa(bin);
}

let videoUrl: Promise<string | null> | null = null;
/** ~2 s animated WebM drawn on a canvas and recorded in the browser (null when MediaRecorder is missing). */
function sampleVideo(): Promise<string | null> {
  videoUrl ??= new Promise((resolve) => {
    try {
      const c = document.createElement("canvas");
      c.width = 320; c.height = 180;
      const g = c.getContext("2d")!;
      const rec = new MediaRecorder(c.captureStream(25), { mimeType: "video/webm" });
      const parts: Blob[] = [];
      rec.ondataavailable = (e) => parts.push(e.data);
      rec.onstop = () => resolve(URL.createObjectURL(new Blob(parts, { type: "video/webm" })));
      const t0 = performance.now();
      const draw = () => {
        const t = (performance.now() - t0) / 1000;
        g.fillStyle = `hsl(${(t * 90) % 360} 60% 40%)`;
        g.fillRect(0, 0, 320, 180);
        g.fillStyle = "#fff";
        g.beginPath(); g.arc(160 + Math.cos(t * 4) * 90, 90 + Math.sin(t * 4) * 40, 22, 0, 7); g.fill();
        g.font = "600 20px sans-serif"; g.fillText(`mock video ${t.toFixed(1)}s`, 12, 28);
        if (t >= 2.2) { clearInterval(iv); rec.stop(); }
      };
      rec.start();
      const iv = setInterval(draw, 40); // not rAF: it is paused in background tabs
    } catch { resolve(null); }
  });
  return videoUrl;
}

async function mockPreview(path: string): Promise<PreviewInfo> {
  await sleep(40 + Math.random() * 60);
  const e = DB.find((x) => x.path === path);
  if (!e) throw new Error("File not found");
  const ext = pvExt(e.name);
  const base: PreviewInfo = {
    kind: "other", name: e.name, path, ext: e.dir ? "" : ext, size: e.dir ? null : e.size, mtime: Math.floor(e.mtime),
    ctime: Math.floor(e.mtime) - 86400000 * 3, image: null, text: null, src: null, thumb: null,
  };
  if (e.dir) return { ...base, kind: "folder" };
  if (PV_IMG.includes(ext)) {
    const lf = libByPath.get(path);
    const w = lf?.w || 1600, h = lf?.h || 1200;
    const shell = ext === "heic";
    const h1 = hash01(path);
    return {
      ...base, kind: "image", src: shell ? null : lf?.tile ? tileUrl(lf.tile, 1024) : svgImage(e.name, w, h),
      image: {
        width: w, height: h, thumb: shell,
        exif: ext === "jpg" || ext === "jpeg" || shell ? { date: "2025-07-1" + Math.floor(h1 * 9) + " 14:22:10", camera: h1 < 0.5 ? "Canon EOS R5" : "Apple iPhone 15 Pro", gps: h1 > 0.4 } : null,
      },
    };
  }
  if (PV_VIDEO.includes(ext)) return { ...base, kind: "video", src: await sampleVideo() };
  if (PV_AUDIO.includes(ext)) return { ...base, kind: "audio", src: wavUrl() };
  if (ext === "pdf") {
    const engine = pdfiumHave();
    return { ...base, kind: "pdf", pdf: { pages: engine && !/locked|secret/i.test(e.name) ? PV_PAGES(path) : null, engine } };
  }
  if (PV_TEXT.includes(ext)) return { ...base, kind: "text", text: { content: sampleText(e.name, ext), truncated: false, encoding: "UTF-8" } };
  return base;
}

export const mockBackend: Backend = {
  async search({ query, offset, limit, sort, filter }) {
    const t0 = performance.now();
    const all = query.toLowerCase().split(/\s+/).filter(Boolean);
    const exts = all.filter((t) => t.startsWith("ext:")).flatMap((t) => t.slice(4).split(/[;,]/)); // Tier 0 `ext:pdf`
    const terms = all.filter((t) => !t.startsWith("ext:"));
    const m = DB.filter((e) => okFilter(e, filter) && (!exts.length || exts.some((x) => e.lower.endsWith("." + x))) && terms.every((t) => e.lower.includes(t)));
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
  iconUrl(path, _isDir, thumb, size) {
    const f = libByPath.get(path);
    return f?.tile && thumb ? tileUrl(f.tile, size && size > 300 ? 512 : 256) : null;
  },
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
  async tier1GetIgnore() { return { list: [...(ignoreList ?? IGNORE_DEFAULTS)], defaults: [...IGNORE_DEFAULTS] }; },
  async tier1SetIgnore(list) {
    await sleep(30);
    const seen = new Set<string>();
    ignoreList = list === null ? null
      : list.map((s) => s.trim()).filter((s) => s && !seen.has(s.toLowerCase()) && !!seen.add(s.toLowerCase()));
    return { list: [...(ignoreList ?? IGNORE_DEFAULTS)], defaults: [...IGNORE_DEFAULTS] };
  },
  async tier1GetIndexCode() { return indexCode; },
  async tier1SetIndexCode(on) { await sleep(30); indexCode = on; return indexCode; },
  previewInfo: mockPreview,
  async semanticSearch(query, k) {
    const st = t1Status().state;
    if (st === "unavailable") return { state: "unavailable", hits: [] };
    const textHits = (): SemanticHit[] => {
      // The words found in the "text layer" of PDFs (and OCR text), with the page they are on.
      const words = query.toLowerCase().split(/\s+/).map((t) => t.replace(/[^a-z0-9]/g, "")).filter((t) => t.length > 1);
      if (!words.length) return [];
      const re = new RegExp(`(${words.join("|")})`, "gi");
      const out: SemanticHit[] = [];
      for (const e of DB) {
        if (e.dir || pvExt(e.name) !== "pdf" || !inScope(e.path)) continue;
        for (let i = 0; i < PDF_LINES.length; i++) {
          if (!words.some((w) => PDF_LINES[i].toLowerCase().includes(w)) || hash01(e.path + "|has|" + i) > 0.5) continue;
          out.push({ path: e.path, score: 0, size: e.size, mtime: Math.floor(e.mtime), snippet: PDF_LINES[i].replace(re, "[$1]"), page: pdfPage(e.path, i), source: "text" });
          break;
        }
      }
      return out;
    };
    if (!modelsInstalled) {
      await sleep(30);
      return { state: "models_missing", hits: textHits().slice(0, Math.max(1, Math.min(k, 200))) };
    }
    await sleep(60 + Math.random() * 60);
    const q = query.toLowerCase().trim();
    const terms = q.split(/\s+/).filter(Boolean);
    if (!q) return { state: st, hits: [] };
    // Fake "image embeddings": stable pseudo-scores per (query, file); files whose name also matches a term
    // rank high, so the "name + meaning" chip and the dedupe are visible in the preview.
    // Every third hit also carries OCR-style text, like a screenshot or scanned page would.
    const OCR = ["Invoice no. 2025-0142 total due 240.00 EUR", "Meeting notes: budget review and next steps", "Receipt Paris cafe 12.50 EUR thank you"];
    const hits: SemanticHit[] = DB.filter((e) => e.kind === 1 && inScope(e.path))
      .map((e): SemanticHit => {
        const h = hash01(q + "|" + e.path);
        const text = hash01(e.path) < 0.34 ? OCR[Math.floor(hash01(e.path + "t") * OCR.length)] : null;
        const snippet = text && terms.length ? text.replace(new RegExp(`(${terms.map((t) => t.replace(/[^a-z0-9]/g, "")).filter(Boolean).join("|") || "$^"})`, "gi"), "[$1]") : null;
        // The OCR-style hits also say where the words sit on the picture: two highlight boxes.
        const by = 0.12 + 0.5 * hash01(e.path + "|box");
        const boxes = snippet ? [{ x: 0.12, y: by, w: 0.42, h: 0.06 }, { x: 0.12, y: by + 0.12, w: 0.58, h: 0.06 }] : null;
        return {
          path: e.path, score: 0.1 + 0.22 * h + (terms.some((t) => e.lower.includes(t)) ? 0.2 : 0),
          size: e.size, mtime: Math.floor(e.mtime), snippet: snippet && snippet.includes("[") ? snippet : null,
          boxes: snippet && snippet.includes("[") ? boxes : null,
        };
      })
      .concat(textHits().map((h) => ({ ...h, score: 0.2 + 0.25 * hash01(h.path), source: "meaning" as const })))
      .sort((a, b) => b.score - a.score)
      .slice(0, Math.max(1, Math.min(k, 200)));
    return { state: st, hits };
  },
  async parserPresent() { await sleep(20); return parserHave(); },
  async parseDocument(path, page): Promise<ParsedDoc> {
    if (!parserHave()) throw new Error("The document parser is not installed. Add it in Setup.");
    const key = `${path}#${page ?? 0}`;
    const cached = parsedCache.has(key);
    await sleep(cached ? 40 : 800);
    parsedCache.add(key);
    lastParseAt = performance.now();
    const two = !!page && page % 2 === 0;
    return {
      markdown: two ? `${MOCK_MARKDOWN}\n\n| Month | Total |\n| --- | --- |\n| March | 1,869.00 |` : MOCK_MARKDOWN,
      tables: two ? [MOCK_TABLE_A, MOCK_TABLE_B] : [MOCK_TABLE_A],
      elements_count: two ? 5 : 4,
      cached,
    };
  },
  async copyTextHtml(text, html) {
    console.info("[mock] copy text", { text, html });
    try { await navigator.clipboard.writeText(text); } catch { /* clipboard may be blocked in a plain browser */ }
  },
  async getProfile() { await sleep(20); return mockProfile; },
  async setProfile(profile) { await sleep(40); mockProfile = profile; return mockProfile; },
  async previewPdfPage(path, page, width) {
    await sleep(60 + Math.random() * 80);
    const e = DB.find((x) => x.path === path);
    if (!e) throw new Error("File not found");
    const pages = PV_PAGES(path);
    if (page < 1 || page > pages) throw new Error(`page ${page} of ${pages}`);
    return new Blob([svgPdfPage(e.name, page, pages, width)], { type: "image/svg+xml" });
  },
  async statusPage() { await sleep(20); return mockStatusPage(); },
  async rescanVolume(letter) {
    await sleep(60);
    if (svc !== "running" || params.get("searchd") === "down") throw new Error("The name service is not running");
    if (!VOLS.some((v) => v.letter === letter)) throw new Error(`volume ${letter}: is not indexed`);
    rescanAt[letter] = performance.now();
  },
  async tier1Rebuild() {
    await sleep(120);
    if (params.get("unavailable") === "1") throw new Error("Search by meaning is unavailable");
    indexed = 0;
    indexTick = performance.now();
    rateSamples = [];
  },
  async modelsPlan(): Promise<ModelsPlan> {
    await sleep(MOCK_BACKEND === "cpu" ? 100 : 600); // GPU detection takes a moment
    const items: ModelsPlan["items"] = [
      ...MOCK_ITEMS.map((i) => ({ name: i.name, bytes: i.bytes, present: modelsInstalled || itemHave(i) })),
      ...PARSER_ITEMS.map((i) => ({ name: i.name, bytes: i.bytes, present: parserHave(), optional: true })),
    ];
    return {
      backend: MOCK_BACKEND,
      gpu: MOCK_BACKEND === "cuda12" ? "NVIDIA GeForce RTX 4070" : MOCK_BACKEND === "vulkan" ? "AMD Radeon RX 6600" : null,
      driver: MOCK_BACKEND === "cuda12" ? "560.94" : null,
      items,
      total_bytes_missing: items.filter((i) => !i.present && !i.optional).reduce((a, i) => a + (i.bytes ?? 0), 0),
      size_unknown: items.some((i) => !i.present && !i.optional && i.bytes == null),
      downloading: dlRunning,
    };
  },
  async modelsDownload(includeParser) {
    if (dlRunning) throw new Error("A download is already running");
    void runDownload(!!includeParser);
  },
  async modelsCancel() { dlCancel = true; },
  async onModelsProgress(cb) { progressCbs.add(cb); return () => { progressCbs.delete(cb); }; },
  async onModelsDone(cb) { doneCbs.add(cb); return () => { doneCbs.delete(cb); }; },
  async onSourceChanged(cb) { listeners.add(cb); return () => { listeners.delete(cb); }; },
  async openLibrary() { window.open(`${location.pathname}?view=library`, "winseek-library"); },
  async libraryImages(): Promise<LibImage[]> {
    await sleep(30);
    const live = livePaths();
    return LIB.filter((f) => f.kind === 1 && live.has(f.path)).sort((a, b) => b.mtime - a.mtime).map((f) => ({ path: f.path, size: f.size, mtime: f.mtime }));
  },
  async libraryCollections() { return collections.map((c) => ({ ...c })); },
  async librarySetCollections(list) {
    await sleep(30);
    const seen = new Set<string>();
    const out = list.map((c) => ({ ...c, name: c.name.trim(), query: c.query.trim(), threshold: Math.max(-1, Math.min(1, c.threshold)) }));
    for (const c of out) {
      if (!c.name || c.name.length > 60) throw new Error("A collection needs a name of 1 to 60 characters");
      if (!c.rule && !c.query) throw new Error(`"${c.name}" needs a search phrase`);
      if (seen.has(c.name.toLowerCase())) throw new Error(`There is already a collection named "${c.name}"`);
      seen.add(c.name.toLowerCase());
    }
    collections = out;
    return out.map((c) => ({ ...c }));
  },
  async libraryCollection(query, threshold, cap) {
    const st = t1Status().state;
    if (!modelsInstalled || st === "unavailable") return { state: st === "unavailable" ? "unavailable" : "models_missing", hits: [] };
    await sleep(80 + Math.random() * 80);
    const q = query.toLowerCase();
    const want = /receipt|invoice/.test(q) ? "receipt" : /whiteboard/.test(q) ? "whiteboard" : /meme|funny/.test(q) ? "meme" : /sign|contract|form/.test(q) ? "document" : /screenshot/.test(q) ? "screenshot" : null;
    const live = livePaths();
    const hits = LIB.filter((f) => f.kind === 1 && f.tile && live.has(f.path))
      .map((f) => {
        const h = hash01(q + "|" + f.path);
        // the range real EmbeddingGemma-2 image/query cosines live in (members ~0.61-0.75, photos ~0.46-0.62)
        const score = want ? (f.tile!.theme === want ? 0.64 + 0.1 * h : 0.46 + 0.14 * h) : 0.46 + 0.2 * h;
        return { path: f.path, score, size: f.size, mtime: Math.floor(f.mtime), snippet: null };
      })
      .filter((h) => h.score >= threshold)
      .sort((a, b) => b.score - a.score)
      .slice(0, Math.max(1, Math.min(cap, 500)));
    return { state: st, hits };
  },
  async libraryDupGroups(): Promise<DupGroup[]> {
    await sleep(40);
    const live = livePaths();
    const by = new Map<string, LibFile[]>();
    for (const f of LIB) if (f.dupKey && live.has(f.path)) by.set(f.dupKey, [...(by.get(f.dupKey) ?? []), f]);
    return [...by.entries()].filter(([, m]) => m.length > 1).map(([id, m]) => {
      const members = m.sort((a, b) => b.mtime - a.mtime).map((f) => ({ path: f.path, size: f.size, mtime: f.mtime, sim: null }));
      return { id, members, reclaimable: members.reduce((a, x) => a + x.size, 0) - Math.max(...members.map((x) => x.size)) };
    }).sort((a, b) => b.reclaimable - a.reclaimable);
  },
  async libraryNearStart() { nearStartedAt = performance.now(); },
  async libraryNearPoll(): Promise<NearState> {
    if (nearStartedAt === null) return { state: "idle", done: 0, total: 0, groups: [], error: null };
    const total = LIB.filter((f) => f.kind === 1).length;
    const frac = Math.min(1, (performance.now() - nearStartedAt) / NEAR_MS);
    if (frac < 1) return { state: "running", done: Math.floor(total * frac), total, groups: [], error: null };
    const live = livePaths();
    const by = new Map<string, LibFile[]>();
    for (const f of LIB) if (f.nearKey && live.has(f.path)) by.set(f.nearKey, [...(by.get(f.nearKey) ?? []), f]);
    const groups: DupGroup[] = [...by.entries()].filter(([, m]) => m.length > 1).map(([id, m]) => {
      const members = m.sort((a, b) => b.mtime - a.mtime).map((f) => ({ path: f.path, size: f.size, mtime: f.mtime, sim: f.tile!.variant === 0 ? 1 : 0.998 - f.tile!.variant * 0.01 }));
      return { id, members, reclaimable: members.reduce((a, x) => a + x.size, 0) - Math.max(...members.map((x) => x.size)) };
    }).sort((a, b) => b.members.length - a.members.length || b.members[0].mtime - a.members[0].mtime);
    return { state: "done", done: total, total, groups, error: null };
  },
  async libraryBigFiles(limit) {
    await sleep(40);
    return DB.filter((e) => !e.dir && e.path.startsWith("C:\\Users\\User\\")).sort((a, b) => b.size - a.size).slice(0, limit).map(toHit);
  },
  async libraryDims(paths) { return paths.map((p) => { const f = libByPath.get(p); return f && f.w ? [f.w, f.h] as [number, number] : null; }); },
  async onOpenStatus() { return () => {}; },
};
