// The "Narrow your search" guide: every filter the search bar understands, as one catalog the
// popover renders and a click edits into the query. Pure (type-only import) so it runs under
// `node --test`. Plain words ride the natural-language parser (chips); the Advanced rows are raw
// index syntax that passes through the parser untouched (see crates/index/src/query.rs).
import type { ChipType, Parsed } from "./filters";

export interface GuideTerm {
  /** What the button shows. */
  label: string;
  /** What lands in the query (an existing chip of the same `type` is replaced / toggled off). */
  text: string;
  /** Chip type this term produces: picking it replaces the previous chip of that type. */
  type?: ChipType;
  /** One-line hint (tooltip + the Advanced rows). */
  hint?: string;
}

export interface GuideGroup {
  id: "kind" | "date" | "size" | "ext" | "advanced";
  title: string;
  blurb: string;
  terms: GuideTerm[];
}

export const GUIDE: GuideGroup[] = [
  {
    id: "kind", title: "Type", blurb: "Only one kind of thing",
    terms: [
      { label: "Pictures", text: "photos", type: "kind" },
      { label: "Documents", text: "documents", type: "kind" },
      { label: "Video", text: "videos", type: "kind" },
      { label: "Audio", text: "music", type: "kind" },
      { label: "Folders", text: "folders", type: "kind" },
      { label: "Apps", text: "apps", type: "kind" },
    ],
  },
  {
    id: "date", title: "Date", blurb: "When it last changed",
    terms: [
      { label: "Today", text: "today", type: "date" },
      { label: "Yesterday", text: "yesterday", type: "date" },
      { label: "Past 7 days", text: "past 7 days", type: "date" },
      { label: "This week", text: "this week", type: "date" },
      { label: "Last week", text: "last week", type: "date" },
      { label: "This month", text: "this month", type: "date" },
      { label: "Last month", text: "last month", type: "date" },
      { label: "This year", text: "this year", type: "date" },
      { label: "Last summer", text: "last summer", type: "date", hint: "Also: spring, autumn, winter, a month (\u201cmarch 2025\u201d) or a year (\u201c2024\u201d)" },
    ],
  },
  {
    id: "size", title: "Size", blurb: "How big it is",
    terms: [
      { label: "Small \u00b7 under 1 MB", text: "small", type: "size" },
      { label: "Big \u00b7 over 100 MB", text: "big", type: "size" },
      { label: "Huge \u00b7 over 1 GB", text: "huge", type: "size" },
      { label: "Over 10 MB", text: "over 10 mb", type: "size", hint: "Any number: \u201cunder 500 kb\u201d, \u201c> 2 gb\u201d" },
    ],
  },
  {
    id: "ext", title: "File type", blurb: "By extension",
    terms: ["pdf", "docx", "xlsx", "pptx", "csv", "txt", "png", "jpg", "mp4", "mp3", "zip", "exe"].map((e) => ({ label: `.${e}`, text: e, type: "ext" as const })),
  },
  {
    id: "advanced", title: "Power syntax", blurb: "Type these straight into the search box",
    terms: [
      { label: "\u201cexact phrase\u201d", text: "\"invoice march\"", hint: "Words together, in this order" },
      { label: "!word", text: "!draft", hint: "Leave out anything matching" },
      { label: "*.log", text: "*.log", hint: "Wildcards: * any run, ? one character" },
      { label: "ext:jpg;png", text: "ext:jpg;png", hint: "Several extensions at once" },
      { label: "size:>50mb", text: "size:>50mb", hint: "Also <, >=, <=, 10mb..1gb" },
      { label: "folder:", text: "folder:", hint: "Only folders whose name matches" },
      { label: "file:", text: "file:", hint: "Only files, never folders" },
      { label: "ws:", text: "ws:", hint: "Match at the start of a word" },
      { label: "code:", text: "code:", hint: "Search inside source code" },
      { label: "C:\\Users\\", text: "C:\\Users\\", hint: "Limits the folder — pasted JSON/ffmpeg/URL paths are cleaned" },
    ],
  },
];

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The words a chip of `type` consumed (its key is `type:words`). */
function chipWords(parsed: Parsed, type: ChipType): string[] {
  return parsed.chips.filter((c) => c.type === type).map((c) => c.key.slice(type.length + 1));
}

/** Is this term what the query already says (its chip is showing)? */
export function isActive(parsed: Parsed, term: GuideTerm): boolean {
  if (!term.type) return false;
  const w = term.text.toLowerCase();
  return chipWords(parsed, term.type).some((x) => x === w);
}

/** Remove the first case-insensitive occurrence of `words` (any whitespace between them). */
function strip(query: string, words: string): string {
  const re = new RegExp(`(^|\\s)${words.split(/\s+/).map(escape).join("\\s+")}(?=\\s|$)`, "i");
  return query.replace(re, "$1");
}

const tidy = (q: string) => q.replace(/\s+/g, " ").trim();

/** The query after a click on `term`: a typed-chip term replaces its predecessor (or, clicked
 *  again, switches off); a syntax row is appended for the user to finish. */
export function applyTerm(query: string, parsed: Parsed, term: GuideTerm): string {
  if (!term.type) {
    const base = query.trimEnd();
    return (base ? base + " " : "") + term.text;
  }
  let q = query;
  for (const w of chipWords(parsed, term.type)) q = strip(q, w);
  q = tidy(q);
  if (isActive(parsed, term)) return q; // second click = off
  return q ? `${q} ${term.text}` : term.text;
}
