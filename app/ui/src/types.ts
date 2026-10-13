export interface Hit {
  name: string;
  path: string;
  is_dir: boolean;
  size: number | null;
  mtime: number | null; // unix ms
}
export interface SearchResult {
  total: number;
  hits: Hit[];
  elapsed_us: number;
}
export interface VolumeStatus {
  letter: string;
  files: number;
  folders: number;
  state: string;
  /** Scan progress (entries seen / expected); null/absent when not scanning. */
  done?: number | null;
  total?: number | null;
}
export type ServiceState = "not_installed" | "stopped" | "starting" | "running" | "stopping" | "unknown";
export type Tier1State = "models_missing" | "idle" | "indexing" | "paused" | "unavailable";
export type PausedReason = "user" | "battery" | "fullscreen";
export interface Tier1Status {
  state: Tier1State;
  stats: {
    files: number; pending: number; done: number; failed: number; skipped: number; vectors: number;
    /** Text is searchable but the vectors wait for the models ("text searchable, waiting for models"). */
    text_only: number;
  } | null;
  include: string[];
  exclude: string[];
  error: string | null;
  paused_reason: PausedReason | null;
}
export interface SemanticHit {
  path: string;
  /** Cosine similarity of the file's embedding to the query. */
  score: number;
  /** On-disk size / modified time (unix ms), when the file could be stat-ed. */
  size?: number | null;
  mtime?: number | null;
  /** Extracted text (OCR / PDF / text file) around the query words, with matches wrapped in [ ]. */
  snippet?: string | null;
  /** PDF page (1-based) the hit is on. */
  page?: number | null;
  /** "text": found in the extracted text only (e.g. before the models are installed); default "meaning". */
  source?: "meaning" | "text";
}
export interface SemanticResult {
  state: string;
  hits: SemanticHit[];
}
export interface PlanItem {
  name: string;
  /** null: the manifest has no size for this item (llama.cpp zips). */
  bytes: number | null;
  present: boolean;
}
/** Tier 1 ignore list: folder/file names skipped anywhere inside the indexed folders. */
export interface IgnoreList {
  list: string[];
  defaults: string[];
}
export interface ModelsPlan {
  backend: "cuda12" | "vulkan" | "cpu";
  gpu: string | null;
  driver: string | null;
  items: PlanItem[];
  total_bytes_missing: number;
  size_unknown: boolean;
  downloading: boolean;
}
export interface ModelsProgress {
  item: string;
  downloaded: number;
  total: number | null;
  overall_downloaded: number;
  overall_total: number;
}
export interface ModelsDone {
  ok: boolean;
  error: string | null;
}
/** One volume of the name index, as the status page shows it. */
export interface VolumeDetail {
  letter: string;
  files: number;
  folders: number;
  state: "live" | "scanning" | "rescanning" | "error" | "demo";
  done: number | null;
  total: number | null;
  error: string | null;
}
export interface StatusTier0 {
  source: "searchd" | "demo" | "unavailable";
  service: ServiceState;
  uptime_s: number | null;
  volumes: VolumeDetail[];
  can_rescan: boolean;
  /** Tooltip for the disabled Rescan button. */
  rescan_hint: string | null;
}
export interface StatusTier1 {
  state: Tier1State;
  paused_reason: PausedReason | null;
  error: string | null;
  stats: Tier1Status["stats"];
  /** embedded / (embedded + pending + failed), 0..1. */
  coverage: number | null;
  /** Files waiting to be embedded. */
  queue: number;
  /** Files per second over the last minute; null when idle or unknown. */
  rate: number | null;
  eta_s: number | null;
  can_rebuild: boolean;
  /** No Windows OCR language is installed, so screenshot text is not read. */
  ocr_unavailable: boolean;
}
export interface StatusModelFile {
  name: string;
  role: "model" | "mmproj";
  expected_bytes: number;
  /** On-disk size; null when the file is missing. */
  bytes: number | null;
}
/** The pinned pdfium runtime that reads PDFs. */
export interface StatusPdfium {
  build: string;
  present: boolean;
  source: "installed" | "override" | "missing";
  expected_bytes: number;
  /** On-disk size of the DLL; null when missing. */
  bytes: number | null;
}
export interface StatusModels {
  llama_build: string;
  backend: "cuda12" | "vulkan" | "cpu";
  gpu: string | null;
  llama_present: boolean;
  model_id: string;
  profile: string;
  quant: string;
  files: StatusModelFile[];
  pdfium: StatusPdfium;
  server: { running: boolean; port: number | null; unload_in_s: number | null; idle_unload_s: number };
}
export interface StatusPage {
  tier0: StatusTier0;
  tier1: StatusTier1;
  models: StatusModels;
}
export type PreviewKind = "image" | "video" | "audio" | "text" | "pdf" | "folder" | "other";
export interface PreviewExif {
  /** "2024-05-03 14:22:10" */
  date: string | null;
  camera: string | null;
  /** The file carries GPS coordinates (they are not shown). */
  gps: boolean;
}
export interface PreviewImage {
  width: number | null;
  height: number | null;
  /** Show the shell thumbnail (`PreviewInfo.thumb`) instead of the file bytes. */
  thumb: boolean;
  exif: PreviewExif | null;
}
export interface PreviewText {
  content: string;
  truncated: boolean;
  encoding: string;
}
export interface PreviewPdf {
  /** Page count; null when the engine is missing or the file cannot be opened (encrypted, corrupt). */
  pages: number | null;
  /** pdfium is installed (false while it is still being downloaded). */
  engine: boolean;
}
/** `preview_info` result. `src` / `thumb` are URLs filled in by the backend wrapper (null when unavailable). */
export interface PreviewInfo {
  kind: PreviewKind;
  name: string;
  path: string;
  ext: string;
  size: number | null;
  mtime: number | null;
  ctime: number | null;
  image: PreviewImage | null;
  text: PreviewText | null;
  pdf?: PreviewPdf | null;
  /** Streamable file URL (image / video / audio). */
  src: string | null;
  /** Large shell thumbnail / icon URL. */
  thumb: string | null;
}
export type Filter = "all" | "images" | "docs" | "video" | "audio" | "folders" | "apps";
export type Sort = "rel" | "name" | "size" | "date";

export type MenuResult = "rename" | "deleted" | "invoked" | "cancelled";

// ---- library window ----
export interface LibImage {
  path: string;
  size: number;
  /** Modified time, unix ms. */
  mtime: number;
}
/** A saved semantic query. `rule: "screenshots"` selects by file name instead of by meaning. */
export interface Collection {
  name: string;
  query: string;
  /** Minimum cosine score for a file to belong. */
  threshold: number;
  rule?: "screenshots" | null;
}
export interface DupMember {
  path: string;
  size: number;
  mtime: number;
  /** Cosine similarity to the group's newest member (near-duplicates only). */
  sim: number | null;
}
export interface DupGroup {
  id: string;
  /** Newest first. */
  members: DupMember[];
  /** Bytes freed by keeping only the largest member. */
  reclaimable: number;
}
export interface NearState {
  state: "idle" | "running" | "done" | "error";
  done: number;
  total: number;
  groups: DupGroup[];
  error: string | null;
}

export interface Backend {
  search(a: { query: string; offset: number; limit: number; sort: Sort; filter: Filter }): Promise<SearchResult>;
  status(): Promise<VolumeStatus[]>;
  recent(limit: number): Promise<Hit[]>;
  openPath(path: string): Promise<void>;
  revealPath(path: string): Promise<void>;
  copyPath(path: string): Promise<void>;
  copyFiles(paths: string[]): Promise<void>;
  recycle(paths: string[]): Promise<void>;
  restoreRecycled(paths: string[]): Promise<number>;
  renamePath(path: string, newName: string): Promise<string>;
  showContextMenu(path: string, x: number, y: number): Promise<MenuResult>;
  startDrag(paths: string[]): Promise<string>;
  /** Shell icon/thumbnail URL, or null when unavailable (mock) -> SVG glyph. */
  iconUrl(path: string, isDir: boolean, thumb: boolean, size?: number): string | null;
  hide(): Promise<void>;
  onShown(cb: () => void): Promise<() => void>;
  serviceState(): Promise<ServiceState>;
  /** Elevated install (one UAC prompt); rejects with a user-readable string. */
  installService(): Promise<void>;
  startService(): Promise<void>;
  /** Windows accent palette (8 x "#rrggbb", index 1 = Light2, 4 = Dark1) or null. */
  accentPalette(): Promise<string[] | null>;
  tier1Status(): Promise<Tier1Status>;
  tier1SetScopes(include: string[], exclude: string[]): Promise<void>;
  /** User pause (wins over the automatic battery / full-screen pause). */
  tier1Pause(paused: boolean): Promise<void>;
  tier1GetIgnore(): Promise<IgnoreList>;
  /** `null` resets to the defaults; an empty list is a valid choice. Returns what was saved. */
  tier1SetIgnore(list: string[] | null): Promise<IgnoreList>;
  /** Opt-in: also index source-code text (honours .gitignore). Default off. */
  tier1GetIndexCode(): Promise<boolean>;
  /** Returns the saved value. Turning it off forgets already indexed code files. */
  tier1SetIndexCode(on: boolean): Promise<boolean>;
  /** Never rejects with anything but a user-readable string ("File not found"). */
  previewInfo(path: string): Promise<PreviewInfo>;
  semanticSearch(query: string, k: number): Promise<SemanticResult>;
  /** One PDF page (1-based) rendered about `width` px wide; PNG (or SVG in the mock) as a Blob. */
  previewPdfPage(path: string, page: number, width: number): Promise<Blob>;
  /** Everything the status page shows. Never rejects because searchd is down (tier0.source says so). */
  statusPage(): Promise<StatusPage>;
  /** Ask searchd to rebuild one volume's name index. */
  rescanVolume(letter: string): Promise<void>;
  /** Drop all embeddings and index the chosen folders again. Never touches the name index. */
  tier1Rebuild(): Promise<void>;
  modelsPlan(): Promise<ModelsPlan>;
  /** Starts the background download; progress arrives via onModelsProgress / onModelsDone. */
  modelsDownload(): Promise<void>;
  /** Aborts but keeps partial files; a later modelsDownload resumes. */
  modelsCancel(): Promise<void>;
  onModelsProgress(cb: (p: ModelsProgress) => void): Promise<() => void>;
  onModelsDone(cb: (d: ModelsDone) => void): Promise<() => void>;
  /** Fired when the app swaps from demo data to the real searchd source. */
  onSourceChanged(cb: () => void): Promise<() => void>;
  /** Show (or create) the library window. */
  openLibrary(): Promise<void>;
  /** Every indexed image, newest first. */
  libraryImages(): Promise<LibImage[]>;
  libraryCollections(): Promise<Collection[]>;
  /** Validates and saves the list; resolves to what was stored. */
  librarySetCollections(list: Collection[]): Promise<Collection[]>;
  /** Best matches of a phrase with score >= threshold (at most `cap`, <= 500). */
  libraryCollection(query: string, threshold: number, cap: number): Promise<SemanticResult>;
  /** Byte-identical files (same content hash), biggest saving first. */
  libraryDupGroups(): Promise<DupGroup[]>;
  /** Start (or restart) the background near-duplicate scan. */
  libraryNearStart(threshold: number): Promise<void>;
  libraryNearPoll(): Promise<NearState>;
  /** The largest files under the user's profile (name index, sorted by size). */
  libraryBigFiles(limit: number): Promise<Hit[]>;
  /** Pixel sizes read from the file headers (null: unknown format). */
  libraryDims(paths: string[]): Promise<([number, number] | null)[]>;
  /** The tray's "Status" item asks the popup to show the status page. */
  onOpenStatus(cb: () => void): Promise<() => void>;
}
