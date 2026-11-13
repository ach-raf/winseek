import type { Hit, SemanticHit } from "./types";

// One ranked list of pictures for a query, whichever way the user got to pictures (the Pictures
// section of the overview, the Pictures tab, a "photos" / "image" word in the query, "Show all").
//
// One final score per picture: what the picture shows is the base (the meaning search's own score,
// on the relevance gate's scale where a hit starts at 4), the file name is a boost. A name counts
// when the typed word is a word of the name or starts one ("cat.jpg", "my cat 2021.png",
// "cat_food.png"); letters that only sit inside another word ("intricate", "location") are not
// evidence that the picture shows a cat, so those go to a separate tail and never above a picture
// that was found by what it shows.

/** How the typed words sit in a file name: each is a whole word, each at least starts a word, or some only sit inside one. */
export type NameMatch = "word" | "start" | "part";

/** Boost of a name whose words are all whole words: above a middling meaning hit, below a confident one. */
export const NAME_WORD = 6;
/** Boost when a word only starts with what was typed ("category" for "cat"): the gate's own floor. */
export const NAME_START = 4;

const lowerKey = (p: string) => p.toLowerCase();
const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const isLetter = (c: string) => /\p{L}/u.test(c);
const isDigit = (c: string) => /\p{N}/u.test(c);
const isAlnum = (c: string) => isLetter(c) || isDigit(c);
const isUpper = (c: string) => c !== c.toLowerCase();
const isLower = (c: string) => c !== c.toUpperCase();

/** Does a word start between `prev` and `cur`? The twin of the name index's rule (`ws:` terms): after
 *  anything that is not a letter or digit, at a lower-to-upper step ("myCat"), where letters meet
 *  digits ("2021cat"), and anywhere in scripts written without spaces. */
export function wordStart(prev: string, cur: string): boolean {
  return !isAlnum(prev)
    || (isLower(prev) && isUpper(cur))
    || (isDigit(prev) !== isDigit(cur) && isAlnum(cur))
    || (cur.charCodeAt(0) > 127 && isLetter(cur) && !isLower(cur) && !isUpper(cur));
}

/** The plain words of a query (lower case): negations, `key:value` terms, paths and wildcards are not words to look for in a name. */
export function queryWords(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/\s+/)) {
    if (!raw || raw.startsWith("!")) continue;
    const t = raw.replace(/^"+|"+$/g, "");
    if (!t || /[:\\/*?]/.test(t)) continue;
    out.push(t.toLowerCase());
  }
  return out;
}

function wordIn(chars: string[], low: string[], word: string): NameMatch | null {
  const w = Array.from(word);
  const n = w.length;
  let best: NameMatch | null = null;
  for (let i = 0; i + n <= chars.length; i++) {
    let j = 0;
    while (j < n && low[i + j] === w[j]) j++;
    if (j < n) continue;
    if (i > 0 && !wordStart(chars[i - 1], chars[i])) { best ??= "part"; continue; }
    // The word ends here, or only a plural ending follows ("cats", "boxes").
    let e = i + n;
    if (low[e] === "e" && low[e + 1] === "s") e += 2; else if (low[e] === "s") e += 1;
    const ends = (at: number) => at >= chars.length || !isAlnum(chars[at]) || wordStart(chars[at - 1], chars[at]);
    if (ends(i + n) || ends(e)) return "word";
    best = "start";
  }
  return best;
}

/** How `words` (from [`queryWords`]) sit in `name`: the weakest of them decides. No words: every name is a full match. */
export function nameMatch(name: string, words: string[]): NameMatch {
  if (!words.length) return "word";
  const chars = Array.from(name);
  const low = chars.map((c) => c.toLowerCase());
  let worst: NameMatch = "word";
  for (const w of words) {
    const m = wordIn(chars, low, w) ?? "part"; // not in the name at all: it matched some other way
    if (m === "part") return "part";
    if (m === "start") worst = "start";
  }
  return worst;
}

/** The words as an index query: each plain word must start a word of the name (`ws:`), the rest stays as typed. */
export function wordQuery(nameQ: string): string {
  return nameQ.split(/\s+/).filter(Boolean).map((t) => (/^[^!":\\/*?]+$/.test(t) ? `ws:${t}` : t)).join(" ");
}

export interface Pic {
  hit: Hit;
  /** Found by what the picture shows (or by the text inside it). */
  sem?: SemanticHit;
  /** Row of the name list it came from, when it did. */
  idx?: number;
  /** How the typed words sit in the file name; null = they are not in it. */
  name: NameMatch | null;
  /** The final score: meaning + name boost. */
  score: number;
  /** `score` relative to the best picture, 1-100; null when nothing was scored (no words typed). */
  pct: number | null;
}

/** A meaning hit's score on the gate's scale (an older answer without it: estimated from the relative one). */
const semRank = (h: SemanticHit): number => h.rank ?? (h.score > 0 ? 4 + 6 * h.score : 0);
const boost = (m: NameMatch | null): number => (m === "word" ? NAME_WORD : m === "start" ? NAME_START : 0);

/**
 * Rank the pictures of a query. `sem`: the pictures the meaning search returned (already gated, best
 * first). `names`: pictures whose file name matched, in the name list's order. A path is listed once.
 * `ranked` is the grid, best first; `tail` holds the names that only contain the letters inside
 * another word and were not found by meaning.
 */
export function rankPictures(o: { sem: SemanticHit[]; names: { hit: Hit; idx?: number }[]; text: string }): { ranked: Pic[]; tail: Pic[] } {
  const words = queryWords(o.text);
  const by = new Map<string, Pic>();
  const semHit = (h: SemanticHit): Hit => ({ name: baseName(h.path), path: h.path, is_dir: false, size: h.size ?? null, mtime: h.mtime ?? null });
  for (const h of o.sem) {
    const k = lowerKey(h.path);
    if (by.has(k)) continue;
    // "part" only when the name list would have matched it too (every word is somewhere in the name).
    const base = baseName(h.path), lowBase = base.toLowerCase();
    const cls = words.length ? nameMatch(base, words) : null;
    const name = cls === "part" && !words.every((w) => lowBase.includes(w)) ? null : cls;
    by.set(k, { hit: semHit(h), sem: h, name, score: semRank(h) + boost(name), pct: null });
  }
  const tail: Pic[] = [];
  const inTail = new Set<string>();
  for (const x of o.names) {
    const k = lowerKey(x.hit.path);
    const cur = by.get(k);
    if (cur) {
      // The name list's own row (size, date, rename / recycle by row) is the better carrier.
      if (cur.idx === undefined && x.idx !== undefined) { cur.idx = x.idx; cur.hit = x.hit; }
      continue;
    }
    const name = nameMatch(x.hit.name, words);
    if (name === "part") {
      if (!inTail.has(k)) { inTail.add(k); tail.push({ hit: x.hit, idx: x.idx, name, score: 0, pct: null }); }
      continue;
    }
    by.set(k, { hit: x.hit, idx: x.idx, name, score: words.length ? boost(name) : 0, pct: null });
  }
  const ranked = Array.from(by.values());
  ranked.sort((a, b) => b.score - a.score); // stable: equal scores keep the order they came in
  const top = ranked[0]?.score ?? 0;
  if (top > 0) for (const p of ranked) p.pct = Math.max(1, Math.min(100, Math.round((p.score / top) * 100)));
  return { ranked, tail };
}

/** Why a picture is in the list, in plain words (tile tooltip, preview). */
export function picReason(p: Pick<Pic, "sem" | "name">, text: string): string | null {
  const words = queryWords(text);
  const q = words.length ? `“${words.join(" ")}”` : "";
  const named = p.name === "word" ? `the name has ${q}` : p.name === "start" ? `a word in the name starts with ${q}` : "";
  const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
  if (p.sem && p.sem.source !== "code") {
    const shows = p.sem.source === "text" ? "Has your words in the picture" : "Matches the picture";
    return named && q ? `${shows}, and ${named}` : shows;
  }
  if (!q) return null;
  if (named) return cap(named);
  return p.name === "part" ? `The name only contains the letters ${q} inside another word` : null;
}

/** The short label on a tile: what kind of evidence put it there. */
export function picBadge(p: Pick<Pic, "sem" | "name">): "both" | "picture" | "text" | "name" | null {
  const named = p.name === "word" || p.name === "start";
  if (p.sem && p.sem.source !== "code") return p.sem.source === "text" ? "text" : named ? "both" : "picture";
  return named ? "name" : null;
}
