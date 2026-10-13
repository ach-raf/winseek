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
  /** Fired when the app swaps from demo data to the real searchd source. */
  onSourceChanged(cb: () => void): Promise<() => void>;
}
