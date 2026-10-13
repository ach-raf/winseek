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
  stats: { files: number; pending: number; done: number; failed: number; skipped: number; vectors: number } | null;
  include: string[];
  exclude: string[];
  error: string | null;
  paused_reason: PausedReason | null;
}
export interface SemanticHit {
  path: string;
  /** Cosine similarity of the file's embedding to the query. */
  score: number;
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
}
export interface StatusModelFile {
  name: string;
  role: "model" | "mmproj";
  expected_bytes: number;
  /** On-disk size; null when the file is missing. */
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
  server: { running: boolean; port: number | null; unload_in_s: number | null; idle_unload_s: number };
}
export interface StatusPage {
  tier0: StatusTier0;
  tier1: StatusTier1;
  models: StatusModels;
}
export type Filter = "all" | "images" | "docs" | "video" | "audio" | "folders";
export type Sort = "rel" | "name" | "size" | "date";

export type MenuResult = "rename" | "deleted" | "invoked" | "cancelled";

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
  iconUrl(path: string, isDir: boolean, thumb: boolean): string | null;
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
  semanticSearch(query: string, k: number): Promise<SemanticResult>;
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
  /** The tray's "Status" item asks the popup to show the status page. */
  onOpenStatus(cb: () => void): Promise<() => void>;
}
