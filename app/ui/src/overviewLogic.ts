import type { Hit, LaunchHit, LaunchItem, SemanticHit } from "./types";
import { pickTop } from "./launchLogic.ts";
import { NAME_START, NAME_WORD, nameHolds, nameMatch, picReason, queryWords, rankPictures, type NameMatch, type Pic } from "./pictureRank.ts";
import { kindOf } from "./format.ts";
import { gridStep } from "./gridLogic.ts";

/** Result overview: a Top result, then a few rows per section with "Show all"; home = Most used + Recent files. */
export type SectionId = "top" | "apps" | "settings" | "commands" | "files" | "images" | "docs" | "weak" | "namepics" | "mostused" | "recent"
  | "k:folder" | "k:doc" | "k:image" | "k:video" | "k:audio" | "k:archive" | "k:other";
/** A section can be opened on its own ("Show all"); files open the full virtualised list instead. */
export type Expand = null | "files" | "images" | "docs" | "apps" | "settings" | "meaning";

/** The section Alt+Right can expand for this item; null when there is no "Show all"
 *  (the Top result and the system commands never have one). */
export function expandable(item: OvItem | undefined): Expand {
  switch (item?.section) {
    case "files": case "images": case "docs": case "apps": case "settings": return item.section;
    default: return null;
  }
}

export interface OvItem {
  key: string;
  section: SectionId;
  hit: Hit;
  /** The meaning / text hit behind this item (pictures, related documents, or a top result found by meaning). */
  sem?: SemanticHit;
  /** Index into the name results, when the item is one. */
  nameIdx?: number;
  /** Column inside the photo grid. */
  col?: number;
  /** The launcher hit behind this row (apps, settings pages, system commands). */
  launch?: LaunchHit;
  /** The calculator Top result carries the result string (Enter copies it). */
  calc?: string;
  /** Pictures: the final score relative to the best picture (1-100), why it is listed, and how the typed words sit in its name. */
  pct?: number | null;
  why?: string | null;
  named?: NameMatch | null;
}

export interface OvSection {
  id: SectionId;
  title: string;
  /** Right-aligned note in the section header ("Found by what the file shows or says, not its name"). */
  note?: string;
  items: OvItem[];
  /** How many there are in all (name results: the searchd total). */
  total: number;
  /** More than shown: offer "Show all". */
  more: boolean;
  /** "Weaker matches" / "Other names containing ...": a folded section; `items` is empty until it is opened. */
  fold?: { open: boolean };
  /** Show `total` next to the title (a section that is the whole list, with no "Show all"). */
  count?: boolean;
  /** `total` is only what was read so far (a quick top-k): "Show all" names no number. */
  open?: boolean;
  /** The optional refiner re-checked this section's top hits (a quiet cue next to the title). */
  refined?: boolean;
  /** A sparse grid (the opened Pictures): `size` positions, `items` only the first of them (the meaning
   *  matches), the rest read by chunk near the viewport. Never looped over: a million positions are normal. */
  sparse?: OvSparse;
}

export interface OvSparse {
  size: number;
  /** The item at position `j` of the section, once its data is in (undefined: an empty placeholder tile). */
  at: (j: number) => OvItem | undefined;
  /** The grid draws positions `[from, to)` of the section now. */
  view?: (from: number, to: number) => void;
}

/** Positions of a section (a sparse grid counts every position, loaded or not). */
export const secSize = (s: OvSection): number => (s.sparse ? s.sparse.size : s.items.length);
/** The item at position `j` of a section, if it is in. */
export const secAt = (s: OvSection, j: number): OvItem | undefined => (s.sparse ? s.sparse.at(j) : s.items[j]);

/** Selection key of a sparse grid's name-matched position `n` (counted after its meaning matches): by position,
 *  so the selection survives its chunk being let go and is there before its data arrives. */
export const gridKey = (n: number): string => "g:" + n;

/**
 * The overview's positions, in order, for selection and keyboard movement. Not an array: a sparse grid has a
 * position for every match and data for only a few thousand of them.
 */
export interface OvNav {
  readonly length: number;
  at(i: number): OvItem | undefined;
  /** The key that selects position `i`: its item's, or the position's own while its data is not in. */
  keyAt(i: number): string | undefined;
  /** Position of a selection key; -1 when it is not in the list. */
  indexOf(key: string): number;
  /** The sections' runs of positions, in order (empty sections left out). */
  readonly spans: readonly NavSpan[];
}
export interface NavSpan { id: SectionId; start: number; size: number }

/** Positions of the given sections. */
export function navOf(sections: OvSection[]): OvNav {
  const runs: (NavSpan & { s: OvSection })[] = [];
  const keys = new Map<string, number>();
  let length = 0;
  for (const s of sections) {
    const size = secSize(s);
    if (!size) continue;
    runs.push({ id: s.id, start: length, size, s });
    s.items.forEach((it, j) => { if (!keys.has(it.key)) keys.set(it.key, length + j); });
    length += size;
  }
  const runOf = (i: number) => runs.find((r) => i >= r.start && i < r.start + r.size);
  return {
    length,
    spans: runs,
    at: (i) => { const r = runOf(i); return r && secAt(r.s, i - r.start); },
    keyAt: (i) => {
      const r = runOf(i);
      if (!r) return undefined;
      const j = i - r.start;
      return secAt(r.s, j)?.key ?? (r.s.sparse && j >= r.s.items.length ? gridKey(j - r.s.items.length) : undefined);
    },
    indexOf: (key) => {
      const at = keys.get(key);
      if (at !== undefined) return at;
      const n = key.startsWith("g:") ? Number(key.slice(2)) : NaN;
      if (!Number.isInteger(n) || n < 0) return -1;
      const r = runs.find((x) => x.s.sparse);
      return r && r.s.items.length + n < r.size ? r.start + r.s.items.length + n : -1;
    },
  };
}

/** Positions of a plain item list (consecutive items of one section form its run). */
function navOfItems(flat: OvItem[]): OvNav {
  const spans: NavSpan[] = [];
  flat.forEach((x, i) => {
    const last = spans[spans.length - 1];
    if (last && last.id === x.section) last.size++;
    else spans.push({ id: x.section, start: i, size: 1 });
  });
  return { length: flat.length, spans, at: (i) => flat[i], keyAt: (i) => flat[i]?.key, indexOf: (k) => flat.findIndex((x) => x.key === k) };
}

/** Photo grid columns in the compact popup (the full window shows 5, the app grid 6 / 8). */
export const GRID_COLS = 4;
const GRID_ROWS = 2;
/** Sections drawn as a tile grid: arrow keys walk them by column. */
export type GridCols = Partial<Record<SectionId, number>>;
export const DEFAULT_COLS: GridCols = { images: GRID_COLS, namepics: GRID_COLS, mostused: 6 };
export const isGrid = (s: SectionId | undefined): boolean => s === "images" || s === "namepics" || s === "mostused";

const FILES_WITH_MEANING = 4;
const FILES_ALONE = 8;
const DOCS = 4;
const APPS = 4;
const SETTINGS = 4;
const COMMANDS = 3;

export const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const stem = (name: string) => { const d = name.lastIndexOf("."); return (d > 0 ? name.slice(0, d) : name).toLowerCase(); };

/** Kind sections of the grouped Files list, in display order. code/exe and the rest share "Other". */
const KIND_GROUPS: { id: SectionId; title: string }[] = [
  { id: "k:folder", title: "Folders" },
  { id: "k:doc", title: "Documents" },
  { id: "k:image", title: "Pictures" },
  { id: "k:video", title: "Videos" },
  { id: "k:audio", title: "Audio" },
  { id: "k:archive", title: "Archives" },
  { id: "k:other", title: "Other files" },
];
const kindSection = (kid: ReturnType<typeof kindOf>): SectionId =>
  kid === "folder" ? "k:folder"
  : kid === "doc" ? "k:doc"
  : kid === "image" ? "k:image"
  : kid === "video" ? "k:video"
  : kid === "audio" ? "k:audio"
  : kid === "archive" ? "k:archive"
  : "k:other";

export const semHit = (h: SemanticHit): Hit => ({ name: baseName(h.path), path: h.path, is_dir: false, size: h.size ?? null, mtime: h.mtime ?? null });

/** Results scoring below this share of the best one are folded under "Weaker matches". */
export const WEAK_RATIO = 0.5;

/** The "NN% match" a hit shows (row badge, photo tile, preview pill): one definition so they agree.
 *  `score` is the final ranking score relative to the best result of the list (1 = the top one), so the
 *  badge reads 100% on top and never rises down the list. A hit the optional refiner re-checked shows
 *  its own 0..1 probability instead. Hits without a score (code, words only
 *  before the models are installed) show none. */
export function matchPct(sem: Pick<SemanticHit, "score" | "source" | "rerank"> | undefined | null): number | null {
  if (sem && sem.source !== "code" && sem.rerank != null) return Math.max(1, Math.min(100, Math.round(sem.rerank * 100)));
  return sem && sem.source !== "code" && sem.score > 0 ? Math.max(1, Math.min(100, Math.round(sem.score * 100))) : null;
}

/** Why a hit is in the list, in plain words, where we know it ("Matches the text on page 3"). */
export function reasonOf(sem: Pick<SemanticHit, "source" | "page" | "at"> | undefined | null, image: boolean): string | null {
  if (!sem || sem.source === "code") return null;
  const clock = (t: number) => `${Math.floor(t / 60)}:${String(Math.floor(t % 60)).padStart(2, "0")}`;
  const where = sem.page ? ` on page ${sem.page}` : sem.at != null ? ` at ${clock(sem.at)}` : "";
  if (sem.source === "text") return `Has your words${where}`;
  if (image) return "Matches the picture";
  return `Matches the ${sem.page ? "text" : "meaning"}${where}`;
}

/** A name result good enough to be the Top result: its name (without extension) is, or starts with, what was typed. */
export function strongName(name: string, text: string): boolean {
  const q = text.trim().toLowerCase();
  if (!q) return false;
  const s = stem(name);
  return s === q || s.startsWith(q) || name.toLowerCase() === q;
}

/** A display Hit for a launch row (icon / title rendering shares the file-row plumbing). */
export function launchHit(h: LaunchHit): Hit {
  const t = h.item.target;
  const path = t.t === "path" ? t.path : t.t === "aumid" ? (t.path ?? `shell:AppsFolder\\${t.aumid}`) : t.t === "uri" ? t.uri : t.t === "exec" ? t.exe : `winseek:${t.name}`;
  return { name: h.item.title, path, is_dir: false, size: null, mtime: null };
}

/** The target path a launch item resolves to (dedupes it against file rows). */
export const launchPath = (h: LaunchHit): string | null => {
  const t = h.item.target;
  return t.t === "path" ? t.path : t.t === "aumid" ? t.path : null;
};

export function buildOverview(opts: {
  names: (Hit | undefined)[]; nameTotal: number; sem: SemanticHit[]; text: string; expand: Expand;
  /** File name -> is a picture (format.kindOf; passed in so this module has no imports). */
  isImage: (name: string) => boolean;
  /** Launcher hits (apps, settings, commands), best first. */
  launch?: LaunchHit[];
  /** Portable exes promoted from the name index (PRD R9). */
  portable?: Hit[];
  /** When the query is pure arithmetic, the calculator result takes the Top result slot. */
  calc?: string;
  /** Columns of the Pictures grid (4 in the popup, 5 in the full window); two rows are shown. */
  photoCols?: number;
  /** Pictures whose name has the typed words as words (the index's `ws:` answer), beyond the rows in `names`. */
  picNames?: Hit[];
  /** How many such pictures there are in all; `picNames` may be only the first of them. */
  picTotal?: number;
  /** Every picture whose name contains the letters, once read (the tail is cut from it); undefined = not read. */
  picTail?: Hit[];
  /** How many pictures contain the letters in their name in all (the Pictures tab count). */
  picNameTotal?: number;
  /** `sem` is a quick top-k and relevant files were left out of it: "Show all" has more. */
  semMore?: boolean;
  /** The optional refiner re-checked the top hits: the meaning section says so. */
  reranked?: boolean;
  /** The folded section of the opened list ("Weaker matches", "Other names containing ...") is unfolded. */
  weakOpen?: boolean;
  /** Group the Files rows into kind sections (Folders / Documents / Pictures / …) with counts and folds. */
  groupByKind?: boolean;
  /** Kind groups that are collapsed; keys are the SectionId minus the "k:" prefix. */
  kindFolds?: Partial<Record<string, boolean>>;
  /** The opened Pictures read their name matches by position (usePicChunks over the `ws:` stream, the meaning
   *  matches excluded): the grid is the meaning matches, then `total` name matches. Without it: a plain list. */
  picGrid?: { total: number | null; at: (n: number) => Hit | undefined; view?: (from: number, to: number) => void };
}): OverviewModel {
  const { names, nameTotal, sem, text, expand, isImage } = opts;
  const launch = opts.launch ?? [];
  const portable = opts.portable ?? [];
  const photoCols = Math.max(1, opts.photoCols ?? GRID_COLS);
  const sections: OvSection[] = [];
  const shownPaths = new Set<string>();
  const k = (p: string) => p.toLowerCase();
  const { picsByMeaning, docsByMeaning } = meaningOnly(sem, text, isImage);

  // Top result: a pure-arithmetic query shows "= result"; else the best launcher item over
  // TOP_MIN (history folded into its score), else an obvious name match, else the best
  // meaning hit when nothing matches by name.
  let top: OvItem | null = null;
  if (!expand && opts.calc !== undefined) {
    const item: LaunchHit = {
      item: { id: "calc", kind: "command", title: `= ${opts.calc}`, subtitle: "Press Enter to copy", keywords: [], target: { t: "cmd", name: "calc" }, icon: null, admin_ok: false },
      score: 0, ranges: [], confirm: false,
    };
    top = { key: "calc", section: "top", hit: { name: `= ${opts.calc}`, path: "winseek:calc", is_dir: false, size: null, mtime: null }, launch: item, calc: opts.calc };
  } else {
    const picked = pickTop(launch, names, text, portable);
    if (!expand) {
      if (picked.launch) {
        top = { key: "l:" + picked.launch.item.id, section: "top", hit: launchHit(picked.launch), launch: picked.launch };
      } else if (picked.file) {
        top = { key: "n0", section: "top", hit: picked.file, nameIdx: picked.nameIdx };
      } else if (names[0] === undefined && nameTotal === 0 && sem.length) {
        const s0 = sem[0]!; // sem.length above guarantees a first hit
        top = { key: "s:" + s0.path, section: "top", hit: semHit(s0), sem: s0 };
      }
    }
  }
  if (top) {
    sections.push({ id: "top", title: "Top result", items: [top], total: 1, more: false });
    shownPaths.add(k(top.hit.path));
    const resolved = top.launch ? launchPath(top.launch) : null;
    if (resolved) shownPaths.add(k(resolved)); // the same exe is not listed again under Files
  }
  const notTop = (h: LaunchHit) => top?.key !== "l:" + h.item.id; // the top result is taken out of its own section

  // Apps: catalog apps first, then the portable exes (they open as normal files). Every app
  // row's real file is kept out of the Files section below (shownPaths).
  if (!expand || expand === "apps") {
    const catApps = launch.filter((h) => h.item.kind === "app" && notTop(h));
    const items: OvItem[] = catApps.map((h): OvItem => ({ key: "l:" + h.item.id, section: "apps", hit: launchHit(h), launch: h }));
    for (const h of portable) {
      if (top && !top.launch && k(h.path) === k(top.hit.path)) continue;
      const idx = names.findIndex((n) => n === h);
      items.push({ key: "p:" + h.path, section: "apps", hit: h, nameIdx: idx >= 0 ? idx : undefined });
    }
    const total = items.length;
    if (items.length) {
      const shown = expand ? items : items.slice(0, APPS);
      for (const it of shown) {
        shownPaths.add(k(it.hit.path));
        if (it.launch) {
          const resolved = launchPath(it.launch);
          if (resolved) shownPaths.add(k(resolved));
        }
      }
      sections.push({ id: "apps", title: "Apps", items: shown, total, more: !expand && total > shown.length });
    }
  }

  const rest = sem.filter((h) => !shownPaths.has(k(h.path)));
  const imgs = rest.filter((h) => isImage(baseName(h.path)));
  // The "By meaning" tab lists every meaning hit (pictures included) in one ranked list, no name rows.
  const docs = expand === "meaning" ? rest : rest.filter((h) => !isImage(baseName(h.path)));
  const hasMeaning = imgs.length + docs.length > 0;

  // Pictures: one ranked list (pictureRank). What the picture shows is the base score and a name
  // that has the typed word as a word is a boost; names that only hold the letters inside another
  // word ("intricate" for "cat") are not in the grid at all: they stay ordinary Files rows in the
  // overview and fold under the grid when the pictures are opened.
  const picSrc: { hit: Hit; idx?: number }[] = [];
  for (let i = 0; i < names.length; i++) {
    const h = names[i];
    if (h && !shownPaths.has(k(h.path)) && isImage(baseName(h.path))) picSrc.push({ hit: h, idx: i });
  }
  // The sparse grid ranks only its meaning matches (name rows still lend them their row) and the fold's names;
  // its name matches come by position instead.
  const sparse = expand === "images" && opts.picGrid ? opts.picGrid : null;
  if (!sparse) for (const h of opts.picNames ?? []) if (!shownPaths.has(k(h.path))) picSrc.push({ hit: h });
  for (const h of opts.picTail ?? []) if (!shownPaths.has(k(h.path))) picSrc.push({ hit: h });
  const pics = rankPictures({ sem: imgs, names: picSrc, text });
  const inGrid = new Set(pics.ranked.map((p) => k(p.hit.path)));
  const wordNamed = (p: Pic) => p.name === "word" || p.name === "start";
  // Pictures the index counts as word matches, and the ranked ones it does not count.
  const namedShown = pics.ranked.filter(wordNamed).length;
  const picTotal = Math.max(namedShown, opts.picTotal ?? 0);
  const picItem = (section: SectionId) => (p: Pic, i: number): OvItem => ({
    key: "i:" + k(p.hit.path), section, hit: p.hit, sem: p.sem, nameIdx: p.idx, col: i % photoCols,
    pct: p.pct, why: picReason(p, text), named: p.name,
  });

  if (!expand) {
    const cap = hasMeaning ? FILES_WITH_MEANING : FILES_ALONE;
    const items: OvItem[] = [];
    for (let i = 0; i < names.length && items.length < cap; i++) {
      const h = names[i];
      if (!h) break;
      if (shownPaths.has(k(h.path))) continue; // the Top result (file or resolved app exe) is not repeated
      if (inGrid.has(k(h.path))) continue; // a picture tile below
      items.push({ key: "n" + i, section: "files", hit: h, nameIdx: i });
      shownPaths.add(k(h.path));
    }
    const hidden = (top?.nameIdx !== undefined ? 1 : 0) + picTotal;
    const total = Math.max(items.length, nameTotal - hidden);
    if (items.length && opts.groupByKind) {
      // Kind sections replace the flat Files list: same rows in a fixed kind order, each with its
      // count in the header and a fold (Spotify's collapsed groups). The whole flat list stays
      // reachable through the Files chip / any sort, which render the column list.
      const buckets = new Map<SectionId, OvItem[]>(KIND_GROUPS.map((g) => [g.id, []]));
      for (const it of items) {
        const kid = kindOf(baseName(it.hit.path), it.hit.is_dir);
        buckets.get(kindSection(kid))!.push(it);
      }
      for (const { id, title } of KIND_GROUPS) {
        const bucket = buckets.get(id)!;
        if (!bucket.length) continue;
        const collapsed = !!opts.kindFolds?.[id.slice(2)];
        sections.push({
          id, title, items: collapsed ? [] : bucket,
          total: bucket.length, more: false, count: true, fold: { open: !collapsed },
        });
      }
    } else if (items.length) {
      sections.push({ id: "files", title: "Files", items, total, more: total > items.length });
    }
  }

  if (sparse) {
    sections.push(...sparsePictures(pics, sparse, { text, photoCols, picNameTotal: opts.picNameTotal, weakOpen: !!opts.weakOpen, picItem }));
  } else if (!expand || expand === "images") {
    // "Show all" is this same list, complete; the grid draws only the tiles near the viewport (Overview's Windowed).
    const list = pics.ranked;
    const cap = expand ? list.length : Math.min(list.length, photoCols * GRID_ROWS);
    const total = Math.max(list.length, picTotal + (list.length - namedShown));
    const items = list.slice(0, cap).map(picItem("images"));
    if (items.length) {
      sections.push({
        id: "images",
        title: "Pictures",
        items,
        total,
        more: !expand && (total > items.length || !!opts.semMore),
        open: !expand && !!opts.semMore,
        count: !!expand,
      });
    }
    const words = queryWords(text);
    if (expand === "images" && words.length) {
      // Names holding the letters inside another word. Counted from the index totals until they are read.
      const semPart = list.filter((p) => p.name === "part").length;
      const tailTotal = opts.picTail ? pics.tail.length : Math.max(pics.tail.length, (opts.picNameTotal ?? 0) - picTotal - semPart);
      if (tailTotal > 0) {
        const open = !!opts.weakOpen || !items.length; // nothing better to show: do not hide the only results
        sections.push({
          id: "namepics", title: `Other names containing \u201c${words.join(" ")}\u201d (${tailTotal.toLocaleString("en-US")})`, total: tailTotal, more: false, fold: { open },
          items: open ? pics.tail.map(picItem("namepics")) : [],
        });
      }
    }
  }

  if (!expand || expand === "docs" || expand === "meaning") {
    const list = docs.filter((h) => !shownPaths.has(k(h.path)));
    // Strong matches first; anything under WEAK_RATIO of the best score is folded ("Weaker matches").
    const strong = list.filter((h) => !(h.score > 0 && h.score < WEAK_RATIO));
    const weak = list.filter((h) => h.score > 0 && h.score < WEAK_RATIO);
    const cap = expand ? strong.length : DOCS;
    const items = strong.slice(0, cap).map((h): OvItem => ({ key: "s:" + h.path, section: "docs", hit: semHit(h), sem: h }));
    if (items.length) {
      sections.push({ id: "docs", title: "Related by meaning", note: "Found by what the file shows or says, not its name", items, total: list.length, refined: !!opts.reranked, more: !expand && (strong.length > items.length || weak.length > 0 || !!opts.semMore), open: !expand && !!opts.semMore });
    }
    if ((expand === "docs" || expand === "meaning") && weak.length) {
      const open = !!opts.weakOpen;
      sections.push({
        id: "weak", title: `Weaker matches (${weak.length.toLocaleString("en-US")})`, total: weak.length, more: false, fold: { open },
        items: open ? weak.map((h): OvItem => ({ key: "s:" + h.path, section: "weak", hit: semHit(h), sem: h })) : [],
      });
    }
  }

  if (!expand || expand === "settings") {
    const list = launch.filter((h) => h.item.kind === "setting" && notTop(h));
    if (list.length) {
      const shown = expand ? list : list.slice(0, SETTINGS);
      sections.push({ id: "settings", title: "Windows settings", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "settings", hit: launchHit(h), launch: h })), total: list.length, more: !expand && list.length > shown.length });
    }
  }

  if (!expand) {
    const list = launch.filter((h) => h.item.kind === "command" && notTop(h));
    if (list.length) {
      const shown = list.slice(0, COMMANDS);
      sections.push({ id: "commands", title: "Commands", items: shown.map((h): OvItem => ({ key: "l:" + h.item.id, section: "commands", hit: launchHit(h), launch: h })), total: list.length, more: list.length > shown.length });
    }
  }

  return { sections, flat: sections.flatMap((s) => s.items), nav: navOf(sections), picsByMeaning, docsByMeaning };
}

export interface OverviewModel {
  sections: OvSection[];
  /** The items held, in order (a sparse grid adds only its meaning matches); `nav` has every position. */
  flat: OvItem[];
  nav: OvNav;
  /** Meaning matches the name index does not count (their name does not hold the typed words): what the type
   *  chips add to the index's counts, so a chip says what its list shows. */
  picsByMeaning: number;
  docsByMeaning: number;
}

/** How many meaning hits (pictures / documents) the name index would not list for `text`: the same rule that
 *  gives a ranked picture no name class (pictureRank's `nameHolds`). */
export function meaningOnly(sem: SemanticHit[], text: string, isImage: (name: string) => boolean): { picsByMeaning: number; docsByMeaning: number } {
  const words = queryWords(text);
  const seen = new Set<string>();
  let picsByMeaning = 0, docsByMeaning = 0;
  for (const h of sem) {
    const key = h.path.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    const name = baseName(h.path);
    if (nameHolds(name, words)) continue;
    if (isImage(name)) picsByMeaning++;
    else if (kindOf(name, false) === "doc") docsByMeaning++;
  }
  return { picsByMeaning, docsByMeaning };
}

/**
 * The opened Pictures as a sparse grid: the meaning matches (ranked, `items`), then every `ws:` name match by
 * position (`picGrid`, the meaning matches already left out by the index). The heading counts the grid and the
 * "Other names containing" fold below it, so it says the same as the Pictures chip: the index's name count
 * plus the meaning matches it does not count.
 */
function sparsePictures(
  pics: { ranked: Pic[]; tail: Pic[] },
  grid: NonNullable<Parameters<typeof buildOverview>[0]["picGrid"]>,
  o: { text: string; photoCols: number; picNameTotal?: number; weakOpen: boolean; picItem: (s: SectionId) => (p: Pic, i: number) => OvItem },
): OvSection[] {
  const words = queryWords(o.text);
  const part1 = pics.ranked.filter((p) => p.sem);
  const items = part1.map(o.picItem("images"));
  const named = part1.filter((p) => p.name !== null).length; // the index counts these by name as well
  const nameTotal = grid.total ?? 0;
  const size = items.length + nameTotal;
  // A name match scores its name boost, on the meaning matches' scale (rankPictures' pct).
  const top = Math.max(part1[0]?.score ?? 0, words.length ? NAME_WORD : 0);
  const built = new Map<Hit, OvItem>();
  const at = (j: number): OvItem | undefined => {
    if (j < items.length) return items[j];
    const n = j - items.length;
    const hit = n < nameTotal ? grid.at(n) : undefined;
    if (!hit) return undefined;
    let it = built.get(hit);
    if (!it) {
      const name = nameMatch(hit.name, words);
      const score = !words.length ? 0 : name === "word" ? NAME_WORD : name === "start" ? NAME_START : 0;
      const pct = top > 0 && score > 0 ? Math.max(1, Math.min(100, Math.round((score / top) * 100))) : null;
      const p: Pic = { hit, name, score, pct };
      it = { ...o.picItem("images")(p, j), key: gridKey(n) };
      built.set(hit, it);
    }
    return it;
  };
  const view = grid.view && ((from: number, to: number) => grid.view?.(Math.max(0, from - items.length), Math.max(0, to - items.length)));
  // The fold: names holding the letters inside another word, counted from the index totals (before they are read).
  const tailTotal = !words.length ? 0
    : o.picNameTotal !== undefined && grid.total !== null ? Math.max(pics.tail.length, o.picNameTotal - nameTotal - named)
    : pics.tail.length;
  const out: OvSection[] = [];
  if (size) {
    out.push({ id: "images", title: "Pictures", items, total: size + tailTotal, more: false, count: true, sparse: { size, at, view } });
  }
  if (tailTotal > 0) {
    const open = o.weakOpen || !size; // nothing better to show: do not hide the only results
    out.push({
      id: "namepics", title: `Other names containing “${words.join(" ")}” (${tailTotal.toLocaleString("en-US")})`, total: tailTotal, more: false, fold: { open },
      items: open ? pics.tail.map(o.picItem("namepics")) : [],
    });
  }
  return out;
}

// ---- home: "Most used" apps + settings (a stored launch count); a drive-scoped home lists that drive's recent files ----

export interface MruEntry { item: LaunchItem; n: number; t: number }
export const MRU_MAX = 24;
/** Rows of tiles the home shows. */
export const HOME_ROWS = 3;

/** Most launched first; ties: most recent first (the one order bump/restore must agree on). */
const mruOrder = (a: MruEntry, b: MruEntry): number => b.n - a.n || b.t - a.t;

/** Count one more launch of `item`; most launched first (ties: most recent first), at most MRU_MAX. */
export function bumpMru(list: MruEntry[], item: LaunchItem, now: number): MruEntry[] {
  const cur = list.find((e) => e.item.id === item.id);
  const next = cur ? list.map((e) => (e === cur ? { item, n: e.n + 1, t: now } : e)) : [...list, { item, n: 1, t: now }];
  return next.sort(mruOrder).slice(0, MRU_MAX);
}

/** Drop the entry for `id` — its launch weight goes with it (used again, it starts fresh at 1). */
export function removeMru(list: MruEntry[], id: string): MruEntry[] {
  return list.filter((e) => e.item.id !== id);
}

/** Undo a "Remove from this list": reinstate `back` (skipping ids the user has since launched again),
 *  most launched first (ties: most recent first), at most MRU_MAX. */
export function restoreMru(list: MruEntry[], back: MruEntry[]): MruEntry[] {
  const ids = new Set(back.map((e) => e.item.id));
  return [...list.filter((e) => !ids.has(e.item.id)), ...back].sort(mruOrder).slice(0, MRU_MAX);
}

/** Home (empty query): "Most used" apps and settings as a tile grid (at most `maxTiles`). Only a drive-scoped
 *  home passes `recent`: that drive's recently changed files follow as rows. */
export function buildHome(opts: {
  mru: LaunchHit[]; recent?: (Hit | undefined)[]; maxTiles: number; recentCap?: number;
}): { sections: OvSection[]; flat: OvItem[]; nav: OvNav } {
  const sections: OvSection[] = [];
  const seen = new Set<string>();
  const apps = opts.mru.filter((h) => !seen.has(h.item.id) && seen.add(h.item.id)).slice(0, Math.max(1, opts.maxTiles));
  if (apps.length) {
    sections.push({
      id: "mostused", title: "Most used", total: apps.length, more: false,
      items: apps.map((h): OvItem => ({ key: "l:" + h.item.id, section: "mostused", hit: launchHit(h), launch: h })),
    });
  }
  const files: OvItem[] = [];
  const recent = opts.recent ?? [];
  for (let i = 0; i < recent.length && files.length < (opts.recentCap ?? 0); i++) {
    const h = recent[i];
    if (h) files.push({ key: "n" + i, section: "recent", hit: h, nameIdx: i });
  }
  if (files.length) sections.push({ id: "recent", title: "Recent files", items: files, total: files.length, more: false });
  return { sections, flat: sections.flatMap((x) => x.items), nav: navOf(sections) };
}

/** Well-known, generic launcher queries for a home with no history yet (never personal). */
export const DEFAULT_HOME_QUERIES = ["notepad", "calculator", "paint", "task manager", "settings", "display", "sound", "bluetooth", "wi-fi", "apps"];

/** Example queries on the home ("Try"): static and generic on purpose (never personal), each one
 *  exercises a real parser feature — kind + season, text + month, text + extension, kind + relative date. */
export const EXAMPLE_QUERIES = ["photos from last summer", "invoice from March", "project plan pdf", "docs from this month"];

// ---- display helpers (pure) ----

/** The folder a file is in, home folder dropped, parts joined with " › ": head shrinks with an ellipsis, tail stays.
 *  "C:\Users\Me\Documents\Finance\x.pdf" -> head "Documents", tail " › Finance". */
export function nicePath(path: string): { head: string; tail: string } {
  const i = path.lastIndexOf("\\");
  const dir = i > 0 ? path.slice(0, i) : path;
  const home = /^[A-Za-z]:\\Users\\[^\\]+/i;
  let parts: string[];
  if (home.test(dir)) {
    parts = dir.replace(home, "").split("\\").filter(Boolean);
    if (!parts.length) parts = ["Home"];
  } else parts = dir.split("\\").filter(Boolean);
  if (!parts.length) return { head: "", tail: dir };
  const last = parts[parts.length - 1]!; // non-empty was checked above
  return parts.length === 1 ? { head: "", tail: last } : { head: parts.slice(0, -1).join(" \u203a "), tail: " \u203a " + last };
}

/** Search time for the stat: one decimal below 10 ms ("0.4"), whole milliseconds above. */
export const fmtMs = (ms: number): string => (ms < 10 ? Math.max(0.1, ms).toFixed(1) : String(Math.round(ms)));

/** The drive scope as a Tier 0 path term: `C:\` matches paths that start there (a term with a backslash is a path match). */
export const scopeDrive = (q: string, drive: string): string => (drive ? `${q} ${drive}:\\`.trim() : q);
/** Client-side twin of scopeDrive, for lists the name index did not answer (recent files, meaning hits). */
export const onDrive = (path: string, drive: string): boolean => !drive || path.toLowerCase().startsWith(drive.toLowerCase() + ":");

/**
 * Keyboard movement through the overview. Up / down step row by row; inside a tile grid (Pictures, Most used)
 * they move a whole grid row (leaving the grid at its edges), left / right move one tile. `next` / `prev` jump
 * a section. `cols` = the columns of each grid section as currently laid out.
 */
export function ovMove(list: OvItem[] | OvNav, i: number, dir: "up" | "down" | "left" | "right" | "next" | "prev", cols: GridCols = DEFAULT_COLS): number {
  // By runs of positions, never by items: a sparse grid's position may have no data yet, and walking a
  // million positions one by one per key press is not an option.
  const nav = Array.isArray(list) ? navOfItems(list) : list;
  if (!nav.length) return 0;
  const spans = nav.spans;
  const clamp = (n: number) => Math.max(0, Math.min(nav.length - 1, n));
  const c = clamp(i);
  const si = Math.max(0, spans.findIndex((r) => c >= r.start && c < r.start + r.size));
  const cur = spans[si]!; // a non-empty list has a run holding every clamped position
  if (dir === "next") return spans[si + 1]?.start ?? i;
  if (dir === "prev") return c !== cur.start ? cur.start : si === 0 ? 0 : spans[si - 1]!.start;
  const colsOf = (sec: SectionId) => Math.max(1, cols[sec] ?? 1);
  if (!isGrid(cur.id)) {
    if (dir === "left" || dir === "right") return i;
    if (dir === "down") return clamp(i + 1);
    // Up into a grid lands on the tile in the first column of its last row.
    const p = clamp(i - 1);
    const prev = p < cur.start ? spans[si - 1] : undefined;
    if (prev && isGrid(prev.id) && p !== i) {
      const n = colsOf(prev.id);
      return prev.start + Math.floor((p - prev.start) / n) * n;
    }
    return p;
  }
  const s = cur.start, e = cur.start + cur.size - 1;
  // The same step every picture grid takes (gridLogic); leaving the grid continues in the rows around it.
  const t = gridStep(c - s, cur.size, colsOf(cur.id), dir);
  if (t < 0) return clamp(s - 1);
  if (t > e - s) return clamp(e + 1);
  return s + t;
}

/** The selection key at `i`, or the nearest one that has a key (walked out in both directions).
 *  An arrow press must never die on a position whose key is not there right now (a sparse chunk
 *  not read yet, a list the live index is rebuilding mid-press): the press lands on the closest
 *  row that can hold the selection instead of being swallowed. */
export function ovKeyNear(nav: OvNav, i: number): string | undefined {
  const n = nav.length;
  if (!n) return undefined;
  const c = Math.max(0, Math.min(n - 1, i));
  for (let d = 0; d < n; d++) {
    const key = nav.keyAt(c - d) ?? (c + d < n ? nav.keyAt(c + d) : undefined);
    if (key) return key;
  }
  return undefined;
}

