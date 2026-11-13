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
/** A folder measured on this PC before it is added to the indexed folders. */
export interface FolderEstimate {
  path: string;
  files: number; images: number; pdfs: number; docs: number; code: number; bytes: number;
  /** First-pass seconds; null when nothing could be measured yet. */
  secs: number | null;
  /** The real time is longer than `secs`. */
  lower_bound: boolean;
  /** The folder walk hit its time budget: counts are partial. */
  capped: boolean;
  /** "observed": this PC's own indexing speed; "sampled": timed reading only; "none". */
  basis: "observed" | "sampled" | "none";
  rate: number | null;
}
export interface UpdateState {
  enabled: boolean;
  current: string;
  /** A newer release tag, when one was found. */
  latest: string | null;
  url: string | null;
  /** Unix seconds of the last successful check (0 = never). */
  last_check: number;
}
export interface Opened {
  /** The app that was steered to the position; null when the file opened the plain way. */
  app: string | null;
  positioned: boolean;
}
export interface SemanticHit {
  path: string;
  /** The final score relative to the best hit of the list (1 = the top one). */
  score: number;
  /** The final score itself, on the relevance gate's scale (a meaning hit starts at 4; the words found in the file add a bonus). */
  rank?: number;
  /** On-disk size / modified time (unix ms), when the file could be stat-ed. */
  size?: number | null;
  mtime?: number | null;
  /** Extracted text (OCR / PDF / text file) around the query words, with matches wrapped in [ ]. */
  snippet?: string | null;
  /** PDF page (1-based) the hit is on. */
  page?: number | null;
  /** Audio / video: the moment (seconds) the matching window or frame starts; the preview seeks there. */
  at?: number | null;
  /** "text": found in the extracted text only (e.g. before the models are installed); "code": a hit of a `code:` query (separate code index); default "meaning". */
  source?: "meaning" | "text" | "code";
  /** Where the query words sit on the image / PDF page (fractions 0-1); only boxes that contain a query word. */
  boxes?: HitBox[] | null;
}
/** A rectangle on an image or PDF page, as fractions 0-1 of its size. */
export interface HitBox {
  x: number;
  y: number;
  w: number;
  h: number;
}
/** Performance profile (Settings): how long the image / document models stay on the graphics card. */
export type Profile = "low" | "balanced" | "fast";
/** What "Extract text" returns for an image or one PDF page. */
export interface ParsedDoc {
  markdown: string;
  tables: { html: string; tsv: string }[];
  elements_count: number;
  /** Answered from the in-memory cache (same content and page parsed a moment ago). */
  cached: boolean;
}
export interface SemanticResult {
  state: string;
  hits: SemanticHit[];
  /** Hits in the whole list this page is cut from ("Show all" pages); else the hits returned. */
  total?: number;
  /** More relevant files exist than were returned. */
  more?: boolean;
}
export interface PlanItem {
  name: string;
  /** null: the manifest has no size for this item (llama.cpp zips). */
  bytes: number | null;
  present: boolean;
  /** Not part of the default setup (the document parser): offered as a checkbox, not counted in the total. */
  optional?: boolean;
  /** Which Settings row (component) the item belongs to. */
  group: ModelGroup;
}
/** One component of search by meaning: the llama.cpp build, the embedding model, pdfium, the optional parser. */
export type ModelGroup = "llama" | "embedder" | "pdfium" | "parser";
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
  /** Filesystem ("NTFS", "FAT32", "exFAT", "ReFS"); "" when unknown. */
  fs: string;
  /** How the volume is kept live; "" when unknown. */
  monitor: "usn" | "folder_watch" | "";
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
  /** Queued files that cannot run yet, and what they wait for. */
  waiting: StatusWaiting[];
  /** Failed files grouped by error message, biggest group first. */
  failed_reasons: StatusReason[];
  /** Skipped files grouped by reason code, biggest group first. */
  skipped_reasons: StatusReason[];
  /** The audio / video tool (ffmpeg): where its automatic download stands; null when nothing waits for it. */
  ffmpeg: StatusFfmpeg | null;
}
export interface StatusFfmpeg {
  state: "idle" | "downloading" | "failed";
  downloaded: number;
  total: number;
  error: string | null;
}
export interface StatusWaiting {
  kind: "pdf" | "audio" | "video" | string;
  count: number;
  why: "pdf_reader_missing" | "pdf_reader_broken" | "ffmpeg_missing" | string;
}
export interface StatusReason {
  reason: string;
  count: number;
  /** A few example paths. */
  examples: string[];
  /** Raw technical detail per example, for a tooltip; never shown as the sentence. */
  details?: string[];
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
/** The document parser (Infinity-Parser2) as the status page shows it. */
export interface StatusParser {
  model_id: string;
  /** Model files and a llama build are on disk, so it can run. */
  present: boolean;
  files: StatusModelFile[];
  server: { running: boolean; port: number | null; unload_in_s: number | null; idle_unload_s: number };
  queue: { queued: number; done: number; failed: number; skipped: number };
  /** Pages per second over the last minute of parsing; null when the parser was not recently working. */
  rate: number | null;
  /** Seconds until the parse queue drains at that rate; null when idle or unknown. */
  eta_s: number | null;
  profile: Profile;
  /** Which images go to the parser. Guesses until the evaluation has tuned them. */
  routing: { doc_min: number; doc_margin: number; screenshot_min_chars: number; provisional: boolean };
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
  parser: StatusParser;
}
export interface StatusPage {
  tier0: StatusTier0;
  tier1: StatusTier1;
  models: StatusModels;
}
export type PreviewKind = "image" | "video" | "audio" | "text" | "document" | "pdf" | "folder" | "other";
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
/** "settings" is a launcher-only scope (the name index is not searched); "apps" filters the name
 *  index to exe/msi AND keeps the launcher's app rows; "files" is everything but folders. */
export type Filter = "all" | "files" | "images" | "docs" | "video" | "audio" | "folders" | "apps" | "settings";
export type Sort = "rel" | "name" | "size" | "date" | "name_desc" | "size_asc" | "date_asc" | "type" | "type_desc";

export type MenuResult = "rename" | "deleted" | "invoked" | "cancelled";

// ---- launcher (apps, Windows settings, system commands — mirrors launch::item serde shapes) ----
export type LaunchKind = "app" | "setting" | "command";
export type LaunchTarget =
  | { t: "aumid"; aumid: string; path: string | null }
  | { t: "path"; path: string; args: string | null }
  | { t: "uri"; uri: string }
  | { t: "exec"; exe: string; args: string[] }
  | { t: "cmd"; name: string };
export interface LaunchItem {
  id: string;
  kind: LaunchKind;
  title: string;
  subtitle: string;
  keywords: string[];
  target: LaunchTarget;
  /** Settings glyph (Segoe Fluent Icons char) when there is no file icon. */
  icon: string | null;
  admin_ok: boolean;
}
export interface LaunchHit {
  item: LaunchItem;
  score: number;
  /** Char ranges into the title for highlighting. */
  ranges: [number, number][];
  /** Needs the second-Enter confirmation (shutdown / restart / sign out / empty bin). */
  confirm: boolean;
}
export interface LaunchCfg {
  apps: boolean;
  settings: boolean;
  commands: boolean;
  /** Portable programs found by sweeping the file index for .exe files (backend catalog). */
  portable: boolean;
  path: boolean;
  hide_uninstallers: boolean;
  folders: string[];
  hidden: string[];
  /** Your own shortcuts: alias typed -> text in an app title / exe name. */
  aliases?: Record<string, string>;
}
/** The rebindable global hotkeys; each a canonical combo ("Ctrl+Alt+Space") or null. */
export interface Hotkeys {
  popup: string | null;
  library: string | null;
  clipboard: string | null;
}
/** Agent access: strictly read-only, so the only switch is on/off plus which contents may be read. */
export interface AgentConfig {
  enabled: boolean;
  /** "indexed": only the "Search by meaning" folders; "any": every file winseek can open. */
  content: "indexed" | "any";
}
/** One agent call in the activity log (paths intact, long text args already cut). */
export interface AgentLogEntry {
  ts_ms: number;
  client: string;
  tool: string;
  args: Record<string, unknown>;
  ok: boolean;
  count?: number;
  error?: string;
  ms: number;
}
export interface McpPath {
  path: string;
  exists: boolean;
}
export interface LaunchCounts {
  apps: number;
  settings: number;
  commands: number;
}

// ---- library window ----
export interface LibImage {
  path: string;
  size: number;
  /** Modified time, unix ms. */
  mtime: number;
  /** Pixel size; 0 when the file could not be read. */
  w: number;
  h: number;
  /** "graphic" = app / game / web assets (icons, logos, sprites): hidden unless asked for. */
  class: ImgClass;
}
export type ImgClass = "photo" | "screenshot" | "graphic";
/** A saved semantic query. `rule: "screenshots"` selects by file name instead of by meaning. */
export interface Collection {
  name: string;
  query: string;
  /** Strictness dial (0.4 loose .. 0.9 strict). */
  threshold: number;
  rule?: "screenshots" | null;
  /** Files removed from the collection by hand (full paths); their copies stay out too. */
  exclude?: string[];
  /** Also let icons and app graphics in. */
  graphics?: boolean;
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
  /** The app's own version (Settings › General); null in the browser mock. */
  appVersion(): Promise<string | null>;
  /** Capacity of the given drive letters; a drive that does not answer is left out. */
  driveSpace(letters: string[]): Promise<DriveSpace[]>;
  recent(limit: number): Promise<Hit[]>;
  openPath(path: string): Promise<void>;
  /** Open at a PDF page, a media time (seconds) or a code line, in the default app when it can be steered
   *  (else VLC / mpv / MPC-HC / VS Code / Notepad++ when installed, else the plain open: `positioned` false). */
  openAt(path: string, at: { page?: number; seconds?: number; line?: number }): Promise<Opened>;
  revealPath(path: string): Promise<void>;
  copyPath(path: string): Promise<void>;
  copyFiles(paths: string[]): Promise<void>;
  recycle(paths: string[]): Promise<void>;
  restoreRecycled(paths: string[]): Promise<number>;
  renamePath(path: string, newName: string): Promise<string>;
  showContextMenu(path: string, x: number, y: number): Promise<MenuResult>;
  /** The Windows Properties sheet for one file (modeless; stays up after the popup hides). */
  showProperties(path: string): Promise<void>;
  /** Windows Terminal in the folder (cmd.exe fallback). */
  openTerminal(dir: string): Promise<void>;
  startDrag(paths: string[]): Promise<string>;
  /** Start a native window drag (the bars are the handle). */
  movePopup(): Promise<void>;
  /** Minimize the full window (its title bar button). */
  minimize(): Promise<void>;
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
  /** Remove a folder for good: out of the scopes, its indexed files forgotten. Returns how many files were removed. */
  tier1RemoveFolder(path: string): Promise<number>;
  /** User pause (wins over the automatic battery / full-screen pause). */
  /** Count a folder and time a sample of it on this machine before it is added. */
  tier1Estimate(path: string): Promise<FolderEstimate>;
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
  /** The best `k` hits; with `k` over 200 or an `offset`, a page of every relevant file ("Show all"). */
  semanticSearch(query: string, k: number, offset?: number): Promise<SemanticResult>;
  /** Are the document parser's files installed? The preview offers its actions only then. */
  parserPresent(): Promise<boolean>;
  /** Read an image or one PDF page (1-based, ignored for images) with the document parser. Takes a few seconds. */
  parseDocument(path: string, page?: number): Promise<ParsedDoc>;
  /** Put text on the clipboard, with an HTML twin (a table) when given. */
  copyTextHtml(text: string, html?: string | null): Promise<void>;
  getProfile(): Promise<Profile>;
  /** Saves and applies; resolves to what was saved. */
  setProfile(profile: Profile): Promise<Profile>;
  /** One PDF page (1-based) rendered about `width` px wide; PNG (or SVG in the mock) as a Blob. */
  previewPdfPage(path: string, page: number, width: number): Promise<Blob>;
  /** Where the query words sit on one PDF page's text layer (fractions 0-1), for hits with no stored boxes. */
  previewPdfBoxes(path: string, page: number, query: string): Promise<HitBox[]>;
  /** Everything the status page shows. Never rejects because searchd is down (tier0.source says so). */
  statusPage(): Promise<StatusPage>;
  /** Ask searchd to rebuild one volume's name index. */
  rescanVolume(letter: string): Promise<void>;
  /** Drop all embeddings and index the chosen folders again. Never touches the name index. */
  tier1Rebuild(): Promise<void>;
  /** "Start with Windows" (HKCU Run value). */
  updatesGet(): Promise<UpdateState>;
  /** Daily update check on/off. Nothing is sent while off. */
  updatesSetEnabled(enabled: boolean): Promise<UpdateState>;
  updatesCheckNow(): Promise<UpdateState>;
  /** Open the GitHub release page in the browser. */
  updatesOpenPage(): Promise<void>;
  autostartGet(): Promise<boolean>;
  /** Resolves to the state after the change. */
  autostartSet(on: boolean): Promise<boolean>;
  modelsPlan(): Promise<ModelsPlan>;
  /** Starts the background download (`includeParser`: also the optional document parser; `groups`: only these
   *  components, as a Settings row asks); progress arrives via onModelsProgress / onModelsDone. */
  modelsDownload(includeParser?: boolean, groups?: ModelGroup[]): Promise<void>;
  /** Aborts but keeps partial files; a later modelsDownload resumes. */
  modelsCancel(): Promise<void>;
  /** Try the audio / video tool (ffmpeg) download again now. */
  ffmpegRetry(): Promise<void>;
  onModelsProgress(cb: (p: ModelsProgress) => void): Promise<() => void>;
  onModelsDone(cb: (d: ModelsDone) => void): Promise<() => void>;
  /** Fired when the app swaps from demo data to the real searchd source. */
  onSourceChanged(cb: () => void): Promise<() => void>;
  /** Show (or create) the library window. */
  openLibrary(): Promise<void>;
  /** Back from the library: show the popup again. */
  openPopup(): Promise<void>;
  // ---- launcher ----
  launchSearch(query: string, limit: number): Promise<LaunchHit[]>;
  launchRun(id: string, query: string, admin: boolean): Promise<void>;
  /** Record a pick the UI handled itself (winseek Settings / Library). */
  launchRecord(id: string, query: string): Promise<void>;
  launchReveal(id: string): Promise<void>;
  launchCopy(id: string): Promise<void>;
  launchHide(id: string): Promise<void>;
  launchUnhide(id: string): Promise<void>;
  launchReindex(): Promise<void>;
  launchGetConfig(): Promise<LaunchCfg>;
  /** Saves; the backend re-enumerates on its own when a source changed. */
  launchSetConfig(cfg: LaunchCfg): Promise<LaunchCfg>;
  launchClearHistory(): Promise<void>;
  launchCounts(): Promise<LaunchCounts>;
  /** Fired after a background re-enumeration swapped the catalog. */
  onLaunchCatalogChanged(cb: () => void): Promise<() => void>;
  /** Shell icon URL for a store / start-menu app id. */
  appIconUrl(aumid: string, size: number): string | null;
  /** Every indexed image, newest first. */
  libraryImages(): Promise<LibImage[]>;
  libraryCollections(): Promise<Collection[]>;
  /** Validates and saves the list; resolves to what was stored. */
  librarySetCollections(list: Collection[]): Promise<Collection[]>;
  /** What belongs to a description at this strictness (at most `cap`, <= 500), minus `exclude`. */
  libraryCollection(query: string, threshold: number, cap: number, exclude?: string[], graphics?: boolean): Promise<SemanticResult>;
  /** Byte-identical files (same content hash), biggest saving first. */
  libraryDupGroups(): Promise<DupGroup[]>;
  /** Start (or restart) the background near-duplicate scan. */
  libraryNearStart(threshold: number): Promise<void>;
  libraryNearPoll(): Promise<NearState>;
  /** The largest files under the user's profile (name index, sorted by size). */
  libraryBigFiles(limit: number): Promise<Hit[]>;
  /** Where the space went under a folder ("" = every drive), from the name index; also what the drive itself has free. */
  diskUsage(path: string, limit?: number): Promise<DiskUsage>;
  /** File types and biggest files under a folder (walks the subtree: ask only when shown). */
  diskUsageDetail(path: string, topFiles?: number): Promise<UsageDetail>;
  /** Pixel sizes read from the file headers (null: unknown format). */
  libraryDims(paths: string[]): Promise<([number, number] | null)[]>;
  /** The tray's "Status" item asks the popup to show the status page. */
  onOpenStatus(cb: () => void): Promise<() => void>;
  /** A daily/manual update check finished (the state may have changed). */
  onUpdateState(cb: () => void): Promise<() => void>;
  // ---- agents (strictly read-only MCP access) ----
  agentGetConfig(): Promise<AgentConfig>;
  /** Saves and applies without a restart. */
  agentSetConfig(cfg: AgentConfig): Promise<AgentConfig>;
  /** The last `limit` calls, oldest last. */
  agentLog(limit: number): Promise<AgentLogEntry[]>;
  agentClearLog(): Promise<void>;
  /** Where winseek-mcp.exe lives next to the app (exists=false in dev builds). */
  agentMcpPath(): Promise<McpPath>;
  // ---- view mode (compact popup / full screen) ----
  viewGet(): Promise<import("./viewMode").ViewState>;
  /** Saves what the hotkey opens next time. */
  viewSetDefault(mode: import("./viewMode").ViewMode): Promise<import("./viewMode").ViewState>;
  /** Switches the window now (resize + reposition) without changing the saved default. */
  viewSetCurrent(mode: import("./viewMode").ViewMode): Promise<import("./viewMode").ViewMode>;
  onViewChanged(cb: (m: import("./viewMode").ViewMode) => void): Promise<() => void>;
  // ---- global hotkeys (rebindable) ----
  hotkeysGet(): Promise<Hotkeys>;
  /** Registers the new combination (or unbinds with null); rejects with a user-facing message
   *  when another app owns it — the previous shortcut keeps working. */
  hotkeysSet(which: "popup" | "library" | "clipboard", combo: string | null): Promise<Hotkeys>;
  /** Fired after any binding changed (also from another window). */
  onHotkeysChanged(cb: (h: Hotkeys) => void): Promise<() => void>;
  /** The clipboard hotkey shows the popup with the clipboard text as the query. */
  onSetQuery(cb: (p: { query: string }) => void): Promise<() => void>;
}

/** Bytes on one drive (sidebar card). */
export interface DriveSpace { letter: string; total: number; free: number; /** Volume label; empty when the drive has none. */ label?: string }
/** One child of a folder in the disk-usage view. `kind`: 0 other, 1 pictures, 2 video, 3 audio, 4 documents, 5 archives, 6 code, 7 programs and system (a folder: what most of its bytes are). */
export interface UsageEntry { name: string; path: string; is_dir: boolean; kind: number; size: number; files: number; folders: number; mtime: number | null }
export interface UsageType { ext: string; kind: number; bytes: number; files: number }
export interface DiskUsage {
  path: string;
  size: number;
  files: number;
  folders: number;
  entries: UsageEntry[];
  /** Children the service left out of `entries`, and the bytes they hold together. */
  more: number;
  more_size: number;
  /** Bytes per file category under the folder (index = `UsageEntry.kind`). */
  cats: number[];
  /** Only for the drive list and a drive root: what the drive itself has free. */
  drives: DriveSpace[];
}
export interface UsageDetail { path: string; types: UsageType[]; biggest: Hit[] }
