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
}
export type Filter = "all" | "images" | "docs" | "video" | "audio" | "folders";
export type Sort = "rel" | "name" | "size" | "date";

export interface Backend {
  search(a: { query: string; offset: number; limit: number; sort: Sort; filter: Filter }): Promise<SearchResult>;
  status(): Promise<VolumeStatus[]>;
  recent(limit: number): Promise<Hit[]>;
  openPath(path: string): Promise<void>;
  revealPath(path: string): Promise<void>;
  copyPath(path: string): Promise<void>;
  hide(): Promise<void>;
  onShown(cb: () => void): Promise<() => void>;
}
