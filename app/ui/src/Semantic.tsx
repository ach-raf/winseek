import type { Filter, Hit, SemanticHit } from "./types";
import { matchesRanges, type KindId, type Parsed } from "./filters";
import { kindOf, type Kind } from "./format";

export const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const key = (path: string) => path.toLowerCase();
export { key as pathKey };

/** "files" is handled before this table: every meaning hit is a file, so the Files chip passes all. */
const KIND_OF: Record<Exclude<KindId, "files">, Kind | null> = { images: "image", docs: "doc", video: "video", audio: "audio", apps: "app", folders: null, settings: null };

/** Do the kind in force (`scope`: the tab, or the kind word of the query — see `kindScope`) and the parsed
 *  chips let this meaning hit through? Date / size only count when the hit carries them. */
export function semAccepts(h: SemanticHit, p: Parsed, scope: Filter = p.kind ?? "all"): boolean {
  const name = baseName(h.path);
  // `code:` hits are source files: the kind chips (photos, documents...) do not apply, an extension chip does.
  if (h.source === "code") return !p.ext || name.toLowerCase().endsWith("." + p.ext);
  if (scope !== "all" && scope !== "files" && KIND_OF[scope] !== kindOf(name, false)) return false;
  if (p.ext && !name.toLowerCase().endsWith("." + p.ext) && !(p.ext === "jpg" && name.toLowerCase().endsWith(".jpeg"))) return false;
  const probe = { is_dir: false, size: h.size ?? null, mtime: h.mtime ?? null };
  const known = { date: probe.mtime === null ? null : p.date, size: probe.size === null ? null : p.size };
  return matchesRanges(known, probe);
}

const q = (s: string) => `\u201c${s}\u201d`;

/**
 * "Why it matched" for a name hit: which of the typed words are in the name, which only in the folder path,
 * the extension filter, or (filters only) the chips that selected it. `score` = also found by meaning.
 */
export function whyName(h: Hit, nameQ: string, chips: string[], score?: number): string {
  const lowerName = h.name.toLowerCase();
  const inName: string[] = [], inPath: string[] = [], other: string[] = [];
  let ext = "";
  for (const t of nameQ.split(/\s+/).filter(Boolean)) {
    if (t.startsWith("!")) continue;
    if (t.startsWith("ext:")) { ext = t.slice(4).split(/[;,]/).map((e) => "." + e).join(" "); continue; }
    const c = t.replace(/^"|"$/g, "");
    if (!c || c.includes(":")) continue;
    if (/[*?]/.test(c)) other.push(`pattern ${c}`);
    else if (lowerName.includes(c.toLowerCase())) inName.push(c);
    else inPath.push(c);
  }
  const parts: string[] = [];
  if (inName.length) parts.push(`name ${inName.map(q).join(" ")}`);
  if (inPath.length) parts.push(`path ${inPath.map(q).join(" ")}`);
  parts.push(...other);
  if (ext) parts.push(`type ${ext}`);
  if (!parts.length) parts.push(chips.length ? chips.join(" \u00b7 ") : "name");
  if (score !== undefined) parts.push(`meaning ${score.toFixed(2)}`);
  return parts.join(" \u00b7 ");
}

/** Turn an FTS5 snippet (`... [match] ...`) into text with <mark>s. */
export function Snippet({ text }: { text: string }) {
  const parts = text.replace(/\s+/g, " ").trim().split(/(\[[^\]]*\])/);
  return <>{parts.map((p, i) => (p.startsWith("[") && p.endsWith("]") ? <mark key={i}>{p.slice(1, -1)}</mark> : p))}</>;
}

export function SemanticHint({ onSetup, onDismiss }: { onSetup: () => void; onDismiss: () => void }) {
  return (
    <div className="sem-hint" role="note">
      <span>Search by meaning is off</span>
      <button className="link" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onSetup}>Set up</button>
      <span className="sp" />
      <button className="ibtn" tabIndex={-1} aria-label="Hide this hint" onMouseDown={(e) => e.preventDefault()} onClick={onDismiss}>
        <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
      </button>
    </div>
  );
}
