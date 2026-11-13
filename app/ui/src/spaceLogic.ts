import type { DiskUsage, UsageDetail, UsageEntry } from "./types";

/** File categories, in the order the name service numbers them (see `category` in the index crate). */
export const KINDS = [
  { id: 0, label: "Other" },
  { id: 1, label: "Pictures" },
  { id: 2, label: "Video" },
  { id: 3, label: "Audio" },
  { id: 4, label: "Documents" },
  { id: 5, label: "Archives" },
  { id: 6, label: "Code" },
  { id: 7, label: "Programs and system" },
] as const;

export interface Rect { x: number; y: number; w: number; h: number }
export interface Tile<T> extends Rect { item: T; value: number }

/** Squarified treemap (Bruls et al.). `items` must be sorted biggest first and positive; the rects tile `box` exactly. */
export function squarify<T>(items: { item: T; value: number }[], box: Rect): Tile<T>[] {
  const out: Tile<T>[] = [];
  const total = items.reduce((a, b) => a + b.value, 0);
  if (!items.length || total <= 0 || box.w <= 0 || box.h <= 0) return out;
  const scale = (box.w * box.h) / total;
  let { x, y, w, h } = box;
  let row: { item: T; value: number }[] = [];
  let rowSum = 0;
  const worst = (r: { value: number }[], sum: number, side: number) => {
    const s = sum * scale;
    let mx = 0, mn = Infinity;
    for (const it of r) { const a = it.value * scale; mx = Math.max(mx, a); mn = Math.min(mn, a); }
    const s2 = side * side;
    return Math.max((s2 * mx) / (s * s), (s * s) / (s2 * mn));
  };
  const flush = () => {
    if (!row.length) return;
    const area = rowSum * scale;
    if (w >= h) {
      const rw = area / h;
      let cy = y;
      for (const it of row) { const ih = (it.value * scale) / rw; out.push({ item: it.item, value: it.value, x, y: cy, w: rw, h: ih }); cy += ih; }
      x += rw; w -= rw;
    } else {
      const rh = area / w;
      let cx = x;
      for (const it of row) { const iw = (it.value * scale) / rh; out.push({ item: it.item, value: it.value, x: cx, y, w: iw, h: rh }); cx += iw; }
      y += rh; h -= rh;
    }
    row = []; rowSum = 0;
  };
  for (const it of items) {
    const side = Math.min(w, h);
    if (row.length && worst([...row, it], rowSum + it.value, side) > worst(row, rowSum, side)) flush();
    row.push(it); rowSum += it.value;
  }
  flush();
  return out;
}

/** "C:\Users\Me" -> This PC, C:\, C:\Users, C:\Users\Me (the drive list, "This PC", comes first). */
export function crumbs(path: string): { label: string; path: string }[] {
  const out = [{ label: "This PC", path: "" }];
  if (!path) return out;
  const parts = path.split(/[\\/]/).filter(Boolean);
  let acc = "";
  parts.forEach((p, i) => {
    acc = i === 0 ? `${p}\\` : `${acc.replace(/\\$/, "")}\\${p}`;
    out.push({ label: p, path: acc });
  });
  return out;
}

/** The folder one level up; "" (the drive list) above a drive root. */
export function parentPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 1) return "";
  return parts.length === 2 ? `${parts[0]}\\` : parts.slice(0, -1).join("\\");
}

const low = (p: string) => p.toLowerCase().replace(/[\\/]+$/, "");
export function isHidden(path: string, hidden: ReadonlySet<string>): boolean {
  if (hidden.size === 0) return false;
  const p = low(path);
  for (const h of hidden) { const l = low(h); if (p === l || p.startsWith(l + "\\")) return true; }
  return false;
}

/** The answer minus what was recycled this session (the index needs a moment to notice). */
export function withoutHidden(d: DiskUsage, hidden: ReadonlySet<string>): DiskUsage {
  if (hidden.size === 0) return d;
  const gone = d.entries.filter((e) => isHidden(e.path, hidden));
  if (gone.length === 0) return d;
  const bytes = gone.reduce((a, e) => a + e.size, 0);
  const files = gone.reduce((a, e) => a + (e.is_dir ? e.files : 1), 0);
  const folders = gone.reduce((a, e) => a + (e.is_dir ? e.folders + 1 : 0), 0);
  return {
    ...d,
    entries: d.entries.filter((e) => !isHidden(e.path, hidden)),
    size: Math.max(0, d.size - bytes),
    files: Math.max(0, d.files - files),
    folders: Math.max(0, d.folders - folders),
  };
}

/** The biggest-files list minus what was recycled this session. */
export function detailWithoutHidden(d: UsageDetail, hidden: ReadonlySet<string>): UsageDetail {
  if (hidden.size === 0 || !d.biggest.some((b) => isHidden(b.path, hidden))) return d;
  return { ...d, biggest: d.biggest.filter((b) => !isHidden(b.path, hidden)) };
}

/** Per-category bytes of a folder. */
export function kindTotals(d: DiskUsage): number[] {
  return KINDS.map((k) => d.cats?.[k.id] ?? 0);
}

/** "Windows (C:)" the way Explorer names a drive; "Local Disk (C:)" when it has no label. */
export function driveTitle(letter: string, label?: string): string {
  return `${label && label.trim() ? label.trim() : "Local Disk"} (${letter.toUpperCase()}:)`;
}

const norm = (p: string) => p.replace(/[\\/]+$/, "").toLowerCase();
/** True when `path` is `root` or lies under it. */
export function within(root: string, path: string): boolean {
  const r = norm(root), p = norm(path);
  return p === r || p.startsWith(r + "\\");
}

/** Folders from `root` down to the parent of `path` (both inclusive of root): what must be open to show `path` in the tree. */
export function chainTo(root: string, path: string): string[] {
  if (!within(root, path)) return [];
  const rootParts = root.split(/[\\/]/).filter(Boolean).length;
  const parts = path.split(/[\\/]/).filter(Boolean);
  const out: string[] = [];
  for (let n = rootParts; n < parts.length; n++) out.push(n === 1 ? `${parts[0]}\\` : parts.slice(0, n).join("\\"));
  return out;
}

export interface TreeRow {
  key: string;
  /** node: a file or folder; more: "+ N smaller items"; busy: its children are being read. */
  type: "node" | "more" | "busy";
  depth: number;
  entry?: UsageEntry;
  /** Bytes of the folder this row sits in (for the % bar). */
  base: number;
  open?: boolean;
  more?: { count: number; size: number };
}

const rootEntry = (path: string, d: DiskUsage): UsageEntry => {
  const parts = path.split(/[\\/]/).filter(Boolean);
  const name = parts.length <= 1 ? (parts[0] ?? path) : parts[parts.length - 1];
  return { name, path, is_dir: true, kind: 0, size: d.size, files: d.files, folders: d.folders, mtime: null };
};

/** The visible tree: the root, then each open folder's children (biggest first) under it. */
export function flattenTree(root: string, get: (p: string) => DiskUsage | undefined, open: ReadonlySet<string>): TreeRow[] {
  const rd = get(root);
  if (!rd) return [{ key: "busy:" + root, type: "busy", depth: 0, base: 0 }];
  const out: TreeRow[] = [];
  const walk = (e: UsageEntry, depth: number, base: number) => {
    const isOpen = e.is_dir && open.has(e.path);
    out.push({ key: e.path, type: "node", depth, entry: e, base, open: isOpen });
    if (!isOpen) return;
    const d = get(e.path);
    if (!d) { out.push({ key: "busy:" + e.path, type: "busy", depth: depth + 1, base: e.size }); return; }
    for (const c of d.entries) walk(c, depth + 1, d.size);
    if (d.more > 0) out.push({ key: "more:" + e.path, type: "more", depth: depth + 1, base: d.size, more: { count: d.more, size: d.more_size } });
  };
  walk(rootEntry(root, rd), 0, rd.size);
  return out;
}

export const pct = (part: number, whole: number): number => (whole > 0 ? (part / whole) * 100 : 0);
export function formatPct(p: number): string {
  if (p >= 10) return `${Math.round(p)}%`;
  if (p >= 0.1) return `${p.toFixed(1)}%`;
  return p > 0 ? "<0.1%" : "0%";
}

/** What a tile merges when its area is too small to read. */
export interface Rest { rest: true; count: number; size: number }
export type TileItem = UsageEntry | Rest;

/** Entries that get their own tile; the many tiny ones (and anything the service left out) become one "rest" tile. */
export function tileItems(d: DiskUsage, box: Rect, minArea = 1100): { item: TileItem; value: number }[] {
  const entries = d.entries.filter((e) => e.size > 0);
  const total = entries.reduce((a, e) => a + e.size, 0) + d.more_size;
  if (total <= 0) return [];
  const area = box.w * box.h;
  const out: { item: TileItem; value: number }[] = [];
  let restCount = d.more, restSize = d.more_size;
  for (const e of entries) {
    if ((e.size / total) * area >= minArea) out.push({ item: e, value: e.size });
    else { restCount++; restSize += e.size; }
  }
  if (restSize > 0) out.push({ item: { rest: true, count: restCount, size: restSize }, value: restSize });
  return out;
}

export type { UsageEntry };
