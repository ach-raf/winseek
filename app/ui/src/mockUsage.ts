// Browser-preview disk usage: a small synthetic file system, rolled up the way the name service does it.
import type { DiskUsage, UsageEntry, UsageType } from "./types";

interface N { name: string; kids?: N[]; size?: number; kind?: number }

const MB = 1024 * 1024;
const GB = 1024 * MB;
let seed = 0x2545f491;
const rnd = () => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) / 4294967296; };

const KIND_OF: Record<string, number> = {
  jpg: 1, png: 1, heic: 1, mp4: 2, mkv: 2, mov: 2, mp3: 3, flac: 3, pdf: 4, docx: 4, xlsx: 4, txt: 4, zip: 5, iso: 5, rar: 5,
  rs: 6, js: 6, json: 6, exe: 7, dll: 7, sys: 7, msi: 7, dat: 7,
};
const extOf = (name: string) => { const i = name.lastIndexOf("."); return i > 0 ? name.slice(i + 1).toLowerCase() : ""; };
const kindOf = (name: string) => KIND_OF[extOf(name)] ?? 0;

const f = (name: string, bytes: number): N => ({ name, size: Math.round(bytes) });
const d = (name: string, ...kids: N[]): N => ({ name, kids });
/** `count` files named `<stem> n.ext`, each about `avg` bytes. */
const many = (stem: string, ext: string, count: number, avg: number): N[] =>
  Array.from({ length: count }, (_, i) => f(`${stem} ${i + 1}.${ext}`, avg * (0.3 + rnd() * 1.4)));

const TREES: Record<string, N> = {
  C: d("C:",
    f("pagefile.sys", 6 * GB), f("hiberfil.sys", 3.2 * GB), f("swapfile.sys", 256 * MB),
    d("Windows",
      d("WinSxS", ...many("component", "dll", 60, 90 * MB)),
      d("System32", ...many("driver", "sys", 40, 40 * MB), ...many("lib", "dll", 80, 8 * MB)),
      d("Installer", ...many("package", "msi", 25, 120 * MB)),
      d("Fonts", ...many("font", "dat", 30, 6 * MB))),
    d("Program Files",
      d("Studio Suite", ...many("module", "dll", 70, 30 * MB), f("studio.exe", 410 * MB)),
      d("Game Launcher", ...many("asset", "dat", 24, 160 * MB)),
      d("Media Tools", ...many("codec", "dll", 30, 22 * MB))),
    d("Program Files (x86)", d("Browser", ...many("pack", "dat", 20, 55 * MB)), d("Office", ...many("module", "dll", 50, 18 * MB))),
    d("Users",
      d("User",
        d("Videos", ...many("Holiday clip", "mp4", 14, 1.4 * GB), ...many("Screen recording", "mkv", 22, 600 * MB)),
        d("Pictures", d("Camera Roll", ...many("IMG", "jpg", 400, 4.5 * MB)), d("Screenshots", ...many("Screenshot", "png", 220, 1.6 * MB))),
        d("Downloads", ...many("setup", "exe", 12, 380 * MB), ...many("archive", "zip", 18, 900 * MB), f("disk-image.iso", 5.4 * GB), ...many("paper", "pdf", 40, 6 * MB)),
        d("Documents", ...many("Report", "docx", 80, 2 * MB), ...many("Budget", "xlsx", 40, 3 * MB), ...many("Scan", "pdf", 60, 9 * MB)),
        d("Music", ...many("Track", "mp3", 300, 9 * MB), ...many("Album", "flac", 60, 38 * MB)),
        d("AppData", d("Local", d("Temp", ...many("cache", "dat", 120, 28 * MB)), d("Browser Cache", ...many("entry", "dat", 200, 9 * MB))), d("Roaming", ...many("config", "json", 40, 1 * MB))),
        d("Projects", d("winseek", d("target", ...many("artifact", "dll", 60, 70 * MB)), ...many("module", "rs", 200, 40 * 1024))),
        d("Desktop", ...many("note", "txt", 12, 20 * 1024), f("notes.pdf", 14 * MB))),
      d("Public", ...many("shared", "docx", 6, 1 * MB)))),
  D: d("D:",
    d("Backups", ...many("backup", "zip", 16, 4.2 * GB), f("system-image.iso", 18 * GB)),
    d("Games", d("Open World", ...many("pak", "dat", 40, 1.8 * GB)), d("Racing", ...many("pak", "dat", 22, 1.1 * GB))),
    d("Footage", ...many("Clip", "mov", 90, 2.4 * GB)),
    d("Archive", ...many("old", "rar", 30, 600 * MB))),
  E: d("E:", d("Photos", ...many("DSC", "jpg", 900, 6 * MB)), d("Music", ...many("Song", "mp3", 400, 8 * MB))),
};

const sizeOf = (n: N): number => (n.kids ? n.kids.reduce((a, k) => a + sizeOf(k), 0) : n.size ?? 0);
const countOf = (n: N): [number, number] => {
  if (!n.kids) return [1, 0];
  let files = 0, folders = 0;
  for (const k of n.kids) { const [a, b] = countOf(k); files += a; folders += b + (k.kids ? 1 : 0); }
  return [files, folders];
};
function eachFile(n: N, fn: (leaf: N, path: string) => void, path: string): void {
  for (const k of n.kids ?? []) {
    const p = `${path.replace(/\\$/, "")}\\${k.name}`;
    if (k.kids) eachFile(k, fn, p); else fn(k, p);
  }
}
function dominantKind(n: N): number {
  const by = new Array(8).fill(0) as number[];
  const walk = (x: N) => { for (const k of x.kids ?? []) { if (k.kids) walk(k); else by[kindOf(k.name)] += k.size ?? 0; } };
  walk(n);
  let best = 0;
  for (let i = 1; i < 8; i++) if (by[i] > by[best]) best = i;
  return best;
}

function find(path: string): { node: N; path: string } | null {
  const parts = path.split(/[\\/]/).filter(Boolean);
  const root = TREES[parts[0]?.[0]?.toUpperCase() ?? ""];
  if (!root) return null;
  let node = root;
  for (const p of parts.slice(1)) {
    const next = node.kids?.find((k) => k.kids && k.name.toLowerCase() === p.toLowerCase());
    if (!next) return null;
    node = next;
  }
  return { node, path: parts.length === 1 ? `${parts[0]}\\` : parts.join("\\") };
}

const MTIME = Date.now() - 40 * 86400000;
const GBd = 1024 ** 3;
const DRIVES = [{ letter: "C", total: 931 * GBd, free: 41 * GBd }, { letter: "D", total: 1863 * GBd, free: 612 * GBd }, { letter: "E", total: 119 * GBd, free: 52 * GBd }];

export function mockDiskUsage(path: string, topFiles = 100): DiskUsage {
  if (!path.trim()) {
    const entries: UsageEntry[] = Object.values(TREES).map((t) => {
      const [files, folders] = countOf(t);
      return { name: t.name, path: `${t.name}\\`, is_dir: true, kind: 0, size: sizeOf(t), files, folders, mtime: null };
    }).sort((a, b) => b.size - a.size);
    return { path: "", size: entries.reduce((a, e) => a + e.size, 0), files: entries.reduce((a, e) => a + e.files, 0), folders: entries.reduce((a, e) => a + e.folders, 0), entries, more: 0, more_size: 0, types: [], biggest: [], drives: DRIVES };
  }
  const at = find(path);
  if (!at) throw new Error(`${path} is not in the index.`);
  const kids = at.node.kids ?? [];
  const entries: UsageEntry[] = kids.map((k) => {
    const p = `${at.path.replace(/\\$/, "")}\\${k.name}`;
    const [files, folders] = countOf(k);
    return { name: k.name, path: p, is_dir: !!k.kids, kind: k.kids ? dominantKind(k) : kindOf(k.name), size: sizeOf(k), files, folders, mtime: MTIME - rnd() * 4e10 };
  }).sort((a, b) => b.size - a.size);
  const types = new Map<string, UsageType>();
  const big: { name: string; path: string; size: number }[] = [];
  eachFile(at.node, (leaf, p) => {
    const ext = extOf(leaf.name);
    const t = types.get(ext) ?? { ext, kind: kindOf(leaf.name), bytes: 0, files: 0 };
    t.bytes += leaf.size ?? 0; t.files++;
    types.set(ext, t);
    big.push({ name: leaf.name, path: p, size: leaf.size ?? 0 });
  }, at.path);
  const [files, folders] = countOf(at.node);
  const shown = entries.slice(0, 400);
  return {
    path: at.path, size: sizeOf(at.node), files, folders,
    entries: shown, more: entries.length - shown.length, more_size: entries.slice(400).reduce((a, e) => a + e.size, 0),
    types: [...types.values()].sort((a, b) => b.bytes - a.bytes).slice(0, 40),
    biggest: big.sort((a, b) => b.size - a.size).slice(0, topFiles).map((b) => ({ name: b.name, path: b.path, is_dir: false, size: b.size, mtime: MTIME })),
    drives: /^[A-Za-z]:\\?$/.test(at.path) ? DRIVES.filter((x) => at.path.toUpperCase().startsWith(x.letter)) : [],
  };
}
