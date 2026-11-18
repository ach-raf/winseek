/** Bytes, the one formatter every surface shows sizes with: "512 B", "1.5 KB", "45 GB" (no
 *  trailing ".0"). Was five per-surface formatters that rounded differently. */
const NUM1 = new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 });
const SIZE_UNITS = ["B", "KB", "MB", "GB", "TB"];
export function formatSize(n: number | null): string {
  if (n === null) return "";
  let v = n, i = 0;
  while (v >= 1024 && i < SIZE_UNITS.length - 1) { v /= 1024; i++; }
  return `${i === 0 ? n : NUM1.format(v)} ${SIZE_UNITS[i]}`;
}

/** Month abbreviations ("Jan"…) come from Intl — the one month-name source in the UI. */
const MON_FMT = new Intl.DateTimeFormat("en-US", { month: "short" });
export const MONTH_SHORT = Array.from({ length: 12 }, (_, m) => MON_FMT.format(new Date(2021, m, 15)));

/** Whole calendar days between two timestamps' local dates (positive = past). The shared
 *  arithmetic of formatDate and relDate — they used to be two disagreeing copies. */
function dayDiff(ms: number, now: number): number {
  const day = (t: number) => { const d = new Date(t); return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime(); };
  return Math.round((day(now) - day(ms)) / 86400000);
}

export function formatDate(ms: number | null, now = Date.now()): string {
  if (ms === null) return "";
  const d = new Date(ms), n = new Date(now);
  const diff = dayDiff(ms, now);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (diff === 0) return `Today ${hm}`;
  if (diff === 1) return `Yesterday ${hm}`;
  if (d.getFullYear() === n.getFullYear()) return `${MONTH_SHORT[d.getMonth()]} ${d.getDate()}`;
  return `${MONTH_SHORT[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}`;
}

/** The message of an error-ish rejection ("Error: x" as the backend words it → "x"). One util
 *  instead of the copy that used to sit at the top of half the components. */
export function errText(e: unknown): string {
  return String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");
}

/** A filter chip's count, short enough that the popup's chip row never clips it: 842, 1.3k, 12k, 4.2M. */
export function chipCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 999_500) return n < 1e4 ? (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k" : Math.round(n / 1e3) + "k";
  return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
}

/** "Today", "Yesterday", "3 days ago", "2 weeks ago", "5 months ago", "2 years ago" (calendar days;
 *  months and years stay the coarse 30 / 365-day buckets every surface has always shown). The
 *  relative twin of formatDate, sharing its day math. */
export function relDate(ms: number | null, now = Date.now()): string {
  if (ms === null) return "";
  const d = dayDiff(ms, now);
  if (d <= 0) return "Today";
  if (d === 1) return "Yesterday";
  if (d < 7) return `${d} days ago`;
  const n = (v: number, unit: string) => (v <= 1 ? `Last ${unit}` : `${v} ${unit}s ago`);
  if (d < 60) return n(Math.round(d / 7), "week");
  if (d < 700) return n(Math.round(d / 30), "month");
  return n(Math.round(d / 365), "year");
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

/** "45 s", "12 min", "3 h 05 min", "2 d 4 h": coarse on purpose, for uptimes and ETAs. */
export function formatDuration(s: number): string {
  s = Math.max(0, Math.round(s));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} h ${String(m % 60).padStart(2, "0")} min`;
  return `${Math.floor(h / 24)} d ${h % 24} h`;
}

/** Countdown as a fixed-width clock ("0:59", "1:00", "12:05"): unlike formatDuration the text never
 *  changes width as it ticks, so a row that shows it every second keeps its layout. */
export function formatClock(s: number): string {
  s = Math.max(0, Math.round(s));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** Files per second: "12.4 files/s" above 10, one decimal below. */
export function formatRate(r: number): string {
  return `${r >= 10 ? r.toFixed(0) : r.toFixed(1)} files/s`;
}

/** Document-parser speed as pages per minute (a page takes seconds, so per-minute reads best). */
export function formatPages(perSecond: number): string {
  const perMin = perSecond * 60;
  return `${perMin >= 10 ? perMin.toFixed(0) : perMin.toFixed(1)} pages/min`;
}

/** Percentage that never rounds to 0 or 100 unless it really is: 99.97% -> "99.9%", 0.02% -> "0.1%". */
export function formatPercent(frac: number): string {
  const p = frac * 100;
  if (p <= 0) return "0%";
  if (p >= 100) return "100%";
  return `${Math.min(99.9, Math.max(0.1, Math.floor(p * 10) / 10)).toFixed(1)}%`;
}
