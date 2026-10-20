// Natural filters, parsed locally on every keystroke (pure, no imports so `node --test` can load it).
//   "photos from last summer"  -> Images + Summer 2026
//   "pdf invoice 2025"         -> PDF + 2025, free text "invoice"
//   "screenshots this week"    -> Screenshots + This week
//   "big videos"               -> Over 100 MB + Video
// Every recognised phrase becomes a chip; the rest is the free text. A chip the user removed (its key is in
// `off`) is not a filter any more and its words go back to being plain search text.

export type KindId = "images" | "docs" | "video" | "audio" | "folders" | "apps" | "settings";
export type ChipType = "kind" | "ext" | "date" | "size";

export interface Chip {
  /** Stable while the matched words stay the same, e.g. `date:last summer`. */
  key: string;
  type: ChipType;
  label: string;
}
/** [from, to) in unix ms. */
export interface DateRange { from: number; to: number }
/** Inclusive byte bounds; null = open end. */
export interface SizeRange { min: number | null; max: number | null }

export interface Parsed {
  kind: KindId | null;
  /** A single extension without dot ("pdf"), sent to the name index as `ext:pdf`. */
  ext: string | null;
  date: DateRange | null;
  size: SizeRange | null;
  /** What is left after the filters are taken out; this is what name and meaning search get. */
  text: string;
  chips: Chip[];
}

const KB = 1024, MB = KB * 1024, GB = MB * 1024, TB = GB * 1024;
const DAY = 86400000;

const KIND_WORDS: Record<string, KindId> = {
  photo: "images", photos: "images", picture: "images", pictures: "images", pic: "images", pics: "images",
  image: "images", images: "images",
  doc: "docs", docs: "docs", document: "docs", documents: "docs",
  video: "video", videos: "video", movie: "video", movies: "video", clip: "video", clips: "video",
  audio: "audio", music: "audio", song: "audio", songs: "audio", podcast: "audio", podcasts: "audio",
  folder: "folders", folders: "folders", directory: "folders", directories: "folders",
  app: "apps", apps: "apps", application: "apps", applications: "apps", program: "apps", programs: "apps",
  installer: "apps", installers: "apps",
  setting: "settings", settings: "settings",
};
const KIND_LABEL: Record<KindId, string> = {
  images: "Images", docs: "Docs", video: "Video", audio: "Audio", folders: "Folders", apps: "Apps", settings: "Settings",
};
const EXT_WORDS = new Set([
  "pdf", "docx", "xlsx", "pptx", "csv", "txt", "md", "png", "jpg", "jpeg", "gif", "heic", "mp4", "mkv", "mp3", "flac", "wav", "zip", "exe", "msi",
]);
const SCREENSHOT = new Set(["screenshot", "screenshots"]);

const MONTHS = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
const MON3 = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const monthIndex = (w: string): number => {
  const i = MONTHS.indexOf(w);
  if (i >= 0) return i;
  return w.length === 3 ? MONTHS.findIndex((m) => m.startsWith(w)) : -1;
};
/** name -> [first month 0-11, length in months]. Winter runs Dec -> Feb. */
const SEASONS: Record<string, [string, number, number]> = {
  spring: ["Spring", 2, 3], summer: ["Summer", 5, 3], autumn: ["Autumn", 8, 3], fall: ["Autumn", 8, 3], winter: ["Winter", 11, 3],
};
const FILLERS = new Set(["from", "in", "during", "of", "since", "on"]);

const UNITS: Record<string, number> = { b: 1, k: KB, kb: KB, m: MB, mb: MB, g: GB, gb: GB, t: TB, tb: TB };
const SIZE_WORDS: Record<string, SizeRange> = {
  big: { min: 100 * MB, max: null }, large: { min: 100 * MB, max: null }, bigger: { min: 100 * MB, max: null },
  huge: { min: GB, max: null }, giant: { min: GB, max: null }, massive: { min: GB, max: null },
  small: { min: null, max: MB }, tiny: { min: null, max: MB },
};

function fmtBytes(n: number): string {
  if (n >= TB) return `${+(n / TB).toFixed(1)} TB`;
  if (n >= GB) return `${+(n / GB).toFixed(1)} GB`;
  if (n >= MB) return `${+(n / MB).toFixed(1)} MB`;
  if (n >= KB) return `${+(n / KB).toFixed(1)} KB`;
  return `${n} B`;
}

const day = (y: number, m: number, d: number) => new Date(y, m, d).getTime();
const monthStart = (y: number, m: number) => new Date(y, m, 1).getTime(); // Date normalises m<0 / m>11

interface Found { len: number; range: DateRange; label: string }

/** Season starting in calendar year `y` (winter = Dec y .. Feb y+1). */
function seasonIn(name: string, y: number): DateRange {
  const [, m0, n] = SEASONS[name];
  return { from: monthStart(y, m0), to: monthStart(y, m0 + n) };
}
function seasonLabel(name: string, y: number): string {
  const [label, m0, n] = SEASONS[name];
  return m0 + n > 12 ? `${label} ${y}–${String(y + 1).slice(2)}` : `${label} ${y}`;
}

/** Date phrase at tokens[i], if any. `lt` are lower-cased tokens. `prefixed` = a filler word ("from", "in") came first. */
function dateAt(lt: string[], i: number, now: number, prefixed: boolean): Found | null {
  const t = lt[i], n1 = lt[i + 1], n2 = lt[i + 2];
  const nd = new Date(now);
  const y = nd.getFullYear(), mo = nd.getMonth(), d = nd.getDate();
  const today = day(y, mo, d);
  const range = (len: number, from: number, to: number, label: string): Found => ({ len, range: { from, to }, label });

  if (t === "today") return range(1, today, today + DAY, "Today");
  if (t === "yesterday") return range(1, day(y, mo, d - 1), today, "Yesterday");

  if ((t === "this" || t === "last") && n1) {
    if (n1 === "week") {
      const mon = day(y, mo, d - ((nd.getDay() + 6) % 7));
      return t === "this" ? range(2, mon, mon + 7 * DAY, "This week") : range(2, day(y, mo, new Date(mon).getDate() - 7), mon, "Last week");
    }
    if (n1 === "month") {
      return t === "this" ? range(2, monthStart(y, mo), monthStart(y, mo + 1), "This month") : range(2, monthStart(y, mo - 1), monthStart(y, mo), "Last month");
    }
    if (n1 === "year") {
      return t === "this" ? range(2, monthStart(y, 0), monthStart(y + 1, 0), "This year") : range(2, monthStart(y - 1, 0), monthStart(y, 0), "Last year");
    }
    if (SEASONS[n1]) {
      // this X: latest season that has started (maybe still running); last X: latest one that has ended.
      let sy = y;
      const pick = (yr: number) => seasonIn(n1, yr);
      while (pick(sy).from > now) sy--;
      if (t === "last") while (pick(sy).to > now) sy--;
      return range(2, pick(sy).from, pick(sy).to, seasonLabel(n1, sy));
    }
  }
  // rolling windows: "last 30 days", "past 2 weeks", "past week"
  if ((t === "last" || t === "past") && n1) {
    const num = /^\d{1,3}$/.test(n1) ? parseInt(n1, 10) : 1;
    const unit = /^\d{1,3}$/.test(n1) ? n2 : n1;
    const used = /^\d{1,3}$/.test(n1) ? 3 : 2;
    const u = unit?.replace(/s$/, "");
    if (num > 0 && (u === "day" || u === "week" || u === "month" || u === "year") && !(t === "last" && used === 2)) {
      const from = u === "day" ? day(y, mo, d - num) : u === "week" ? day(y, mo, d - 7 * num) : u === "month" ? day(y, mo - num, d) : day(y - num, mo, d);
      const label = u === "day" && num === 1 ? "Past 24 h" : `Past ${num === 1 ? "" : num + " "}${u}${num === 1 ? "" : "s"}`;
      return range(used, from, today + DAY, label);
    }
  }

  // bare season, optionally with a year: "summer", "summer 2023"
  if (SEASONS[t]) {
    const yr = n1 && /^(19|20)\d\d$/.test(n1) ? parseInt(n1, 10) : null;
    if (yr !== null) return range(2, seasonIn(t, yr).from, seasonIn(t, yr).to, seasonLabel(t, yr));
    if (t === "fall" && !prefixed) return null; // "fall" is also a plain word
    let sy = y;
    while (seasonIn(t, sy).from > now) sy--;
    return range(1, seasonIn(t, sy).from, seasonIn(t, sy).to, seasonLabel(t, sy));
  }

  // month, optionally with a year: "march", "mar 2025"
  const mi = monthIndex(t);
  if (mi >= 0) {
    const yr = n1 && /^(19|20)\d\d$/.test(n1) ? parseInt(n1, 10) : null;
    if (t === "may" && yr === null && !prefixed) return null; // "may" is a plain word
    if (yr !== null) return range(2, monthStart(yr, mi), monthStart(yr, mi + 1), `${MON3[mi]} ${yr}`);
    const my = mi > mo ? y - 1 : y; // the most recent such month that has started
    return range(1, monthStart(my, mi), monthStart(my, mi + 1), `${MON3[mi]} ${my}`);
  }

  if (/^(19|20)\d\d$/.test(t)) {
    const yr = parseInt(t, 10);
    return range(1, monthStart(yr, 0), monthStart(yr + 1, 0), String(yr));
  }
  return null;
}

interface SizeFound { len: number; size: SizeRange; label: string }

const num = (s: string | undefined) => (s && /^\d+(\.\d+)?$/.test(s) ? parseFloat(s) : NaN);

/** ">100mb", "> 100 mb", "<5mb", "over 1gb", "under 500 kb", or the words big / huge / small. */
function sizeAt(lt: string[], i: number): SizeFound | null {
  const t = lt[i];
  const w = SIZE_WORDS[t];
  if (w) {
    return { len: 1, size: w, label: w.min !== null ? `Over ${fmtBytes(w.min)}` : `Under ${fmtBytes(w.max as number)}` };
  }
  let op: string | null = null, at = i + 1;
  const m = /^([<>]=?)(.*)$/.exec(t);
  let rest = "";
  if (m) { op = m[1]; rest = m[2]; }
  else if (["over", "above", "bigger", "larger", "under", "below", "smaller", "less", "more"].includes(t)) {
    op = ["over", "above", "bigger", "larger", "more"].includes(t) ? ">" : "<";
    if (lt[at] === "than") at++;
  } else return null;

  // value: glued ("100mb"), or number then unit ("100 mb"), possibly after the operator token
  const glued = rest || lt[at];
  const g = /^(\d+(?:\.\d+)?)([a-z]{1,2})?$/.exec(glued ?? "");
  if (!g) return null;
  const v = num(g[1]);
  let unit: string | undefined = g[2];
  let end = rest ? at : at + 1;
  if (!unit && lt[end] && UNITS[lt[end]] !== undefined) { unit = lt[end]; end++; }
  if (!unit || UNITS[unit] === undefined || !(v > 0)) return null;
  const bytes = Math.round(v * UNITS[unit]);
  return op.startsWith(">")
    ? { len: end - i, size: { min: bytes, max: null }, label: `Over ${fmtBytes(bytes)}` }
    : { len: end - i, size: { min: null, max: bytes }, label: `Under ${fmtBytes(bytes)}` };
}

export function parseQuery(input: string, opts: { now?: number; off?: ReadonlySet<string> } = {}): Parsed {
  const now = opts.now ?? Date.now();
  const off = opts.off;
  const toks = input.split(/\s+/).filter(Boolean);
  const lt = toks.map((t) => t.toLowerCase());
  const out: Parsed = { kind: null, ext: null, date: null, size: null, text: "", chips: [] };
  const keep: string[] = [];
  let quoted = false;

  const take = (type: ChipType, from: number, len: number, label: string): boolean => {
    const key = `${type}:${lt.slice(from, from + len).join(" ")}`;
    if (off?.has(key)) return false;
    out.chips.push({ key, type, label });
    return true;
  };

  for (let i = 0; i < toks.length; i++) {
    const t = toks[i];
    // quoted phrases, negations, wildcards, path or operator terms are the user's own syntax: leave alone
    if (quoted || t.startsWith('"') || t.startsWith("!") || /[\\/*?]/.test(t) || (t.includes(":") && !t.startsWith(">") && !t.startsWith("<"))) {
      if (!quoted && t.startsWith('"') && !(t.length > 1 && t.endsWith('"'))) quoted = true;
      else if (quoted && t.endsWith('"')) quoted = false;
      keep.push(t);
      continue;
    }

    if (!out.size) {
      const s = sizeAt(lt, i);
      if (s && take("size", i, s.len, s.label)) { out.size = s.size; i += s.len - 1; continue; }
    }
    if (!out.date) {
      // "from last summer": the filler word belongs to the date phrase
      const filler = FILLERS.has(lt[i]) ? 1 : 0;
      const f = dateAt(lt, i + filler, now, filler === 1);
      if (f && take("date", i, f.len + filler, f.label)) { out.date = f.range; i += f.len + filler - 1; continue; }
    }
    if (!out.kind && !out.ext && SCREENSHOT.has(lt[i])) {
      if (take("kind", i, 1, "Screenshots")) { out.kind = "images"; keep.push("screenshot"); continue; }
    }
    if (!out.kind && KIND_WORDS[lt[i]]) {
      const k = KIND_WORDS[lt[i]];
      if (take("kind", i, 1, KIND_LABEL[k])) { out.kind = k; continue; }
    }
    const ew = lt[i].replace(/^\./, "");
    if (!out.ext && EXT_WORDS.has(ew)) {
      if (take("ext", i, 1, ew.toUpperCase())) { out.ext = ew; continue; }
    }
    keep.push(t);
  }
  out.text = keep.join(" ");
  return out;
}

interface Item { is_dir: boolean; size: number | null; mtime: number | null }
/** Backspace removes the last filter chip only when the caret sits at position 0 with no
 *  selection — anywhere else it must keep editing the text (PRD B4). */
export function backspaceDropsChip(caretAtStart: boolean, hasSelection: boolean, chips: number): boolean {
  return caretAtStart && !hasSelection && chips > 0;
}

/** The date and size parts of a parse (kind / ext are handled by the name index and by kindMatches). */
export function matchesRanges(p: Pick<Parsed, "date" | "size">, h: Item): boolean {
  if (p.date && (h.mtime === null || h.mtime < p.date.from || h.mtime >= p.date.to)) return false;
  if (p.size) {
    if (h.size === null) return false;
    if (p.size.min !== null && h.size < p.size.min) return false;
    if (p.size.max !== null && h.size > p.size.max) return false;
  }
  return true;
}

export const hasRanges = (p: Pick<Parsed, "date" | "size">) => !!(p.date || p.size);

/** Sent to the name index: free text plus `ext:` when an extension was named. */
export const nameQuery = (p: Pick<Parsed, "text" | "ext">) => (p.ext ? `${p.text} ext:${p.ext}`.trim() : p.text);
