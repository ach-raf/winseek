export function formatSize(n: number | null): string {
  if (n === null) return "";
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${u[i]}`;
}

const MON = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
export function formatDate(ms: number | null, now = Date.now()): string {
  if (ms === null) return "";
  const d = new Date(ms), n = new Date(now);
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(n) - day(d)) / 86400000);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (diff === 0) return `Today ${hm}`;
  if (diff === 1) return `Yesterday ${hm}`;
  if (d.getFullYear() === n.getFullYear()) return `${MON[d.getMonth()]} ${d.getDate()}`;
  return `${MON[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}`;
}

/** Split a path's directory into a shrinkable head and a fixed tail (last folder), for CSS middle-truncation. */
export function splitDir(path: string): { head: string; tail: string } {
  const i = path.lastIndexOf("\\");
  const dir = i > 0 ? path.slice(0, i) : path;
  const j = dir.lastIndexOf("\\");
  if (j <= 2) return { head: "", tail: dir };
  return { head: dir.slice(0, j), tail: dir.slice(j) };
}

/** Ranges [start,end) of case-insensitive term matches in name, merged. */
export function matchRanges(name: string, query: string): [number, number][] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  const lower = name.toLowerCase();
  const r: [number, number][] = [];
  for (const t of terms) {
    let from = 0, at: number;
    while ((at = lower.indexOf(t, from)) >= 0) { r.push([at, at + t.length]); from = at + t.length; }
  }
  r.sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  for (const x of r) {
    const last = out[out.length - 1];
    if (last && x[0] <= last[1]) last[1] = Math.max(last[1], x[1]); else out.push([x[0], x[1]]);
  }
  return out;
}

export type Kind = "folder" | "image" | "doc" | "video" | "audio" | "archive" | "code" | "app" | "file";
export function kindOf(name: string, isDir: boolean): Kind {
  if (isDir) return "folder";
  const e = name.slice(name.lastIndexOf(".") + 1).toLowerCase();
  if (["jpg","jpeg","png","gif","heic","webp","bmp","svg"].includes(e)) return "image";
  if (["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","md","rtf"].includes(e)) return "doc";
  if (["mp4","mkv","mov","avi","webm"].includes(e)) return "video";
  if (["mp3","flac","wav","m4a","ogg"].includes(e)) return "audio";
  if (["zip","7z","rar","tar","gz"].includes(e)) return "archive";
  if (["rs","py","ts","tsx","js","json","toml","c","cpp","h","log"].includes(e)) return "code";
  if (["exe","msi","dll"].includes(e)) return "app";
  return "file";
}
