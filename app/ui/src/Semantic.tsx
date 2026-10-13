import { memo } from "react";
import type { Hit, SemanticHit } from "./types";
import { matchesRanges, type KindId, type Parsed } from "./filters";
import { FileIcon } from "./FileIcon";
import { kindOf, splitDir, type Kind } from "./format";

export const SEM_ROWS = 8;

export const baseName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);
const key = (path: string) => path.toLowerCase();
export { key as pathKey };

/** Semantic hits not already shown as name results (a file appears once; the name row carries the chip). */
export function dedupe(hits: SemanticHit[], shown: Iterable<string>): SemanticHit[] {
  const seen = new Set<string>();
  for (const p of shown) seen.add(key(p));
  return hits.filter((h) => !seen.has(key(h.path))).slice(0, SEM_ROWS);
}

const KIND_OF: Record<KindId, Kind | null> = { images: "image", docs: "doc", video: "video", audio: "audio", apps: "app", folders: null };

/** Do the parsed chips let this meaning hit through? Date / size only count when the hit carries them. */
export function semAccepts(h: SemanticHit, p: Parsed): boolean {
  const name = baseName(h.path);
  if (p.kind && KIND_OF[p.kind] !== kindOf(name, false)) return false;
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

export const SemRow = memo(function SemRow({ hit, query, selected, icon, thumb, onPick, onHover, onMenu }: {
  hit: SemanticHit; query: string; selected: boolean; icon: string | null; thumb: boolean;
  onPick: () => void; onHover: () => void; onMenu: (e: React.MouseEvent) => void;
}) {
  const name = baseName(hit.path);
  const { head, tail } = splitDir(hit.path);
  const image = kindOf(name, false) === "image";
  return (
    <div className={"row flow" + (selected ? " selected" : "")} role="option" aria-selected={selected}
         data-sel={selected ? "1" : undefined} title={hit.path}
         onClick={onPick} onMouseMove={onHover} onContextMenu={onMenu}>
      <FileIcon name={name} isDir={false} src={icon} thumb={thumb} />
      <div className="meta">
        <div className="name">{name}</div>
        <div className="path"><span className="head">{head}</span><span className="tail">{tail}</span></div>
        {hit.snippet && <div className="snippet" title="Text found inside the file"><Snippet text={hit.snippet} /></div>}
      </div>
      {hit.source === "text" ? (
        <div className="side why" title="The words were found in the text of the file. Search by meaning is not set up yet.">
          <span className="why-text">contains {q(query)}</span>
          <span className="why-score">text{hit.page ? ` \u00b7 page ${hit.page}` : ""}</span>
        </div>
      ) : (
        <div className="side why" title={`Similarity score ${hit.score.toFixed(3)}`}>
          <span className="why-text">{image ? "looks like " : "matches "}{q(query)}</span>
          <span className="why-score">meaning {"\u00b7"} {hit.score.toFixed(2)}{hit.page ? ` \u00b7 page ${hit.page}` : ""}</span>
        </div>
      )}
    </div>
  );
});

export function SemanticHint({ onSetup, onDismiss }: { onSetup: () => void; onDismiss: () => void }) {
  return (
    <div className="sem-hint" role="note">
      <span>Search by meaning is off &mdash; </span>
      <button className="link" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onSetup}>Set up</button>
      <span className="spacer" />
      <button className="x" tabIndex={-1} aria-label="Hide this hint" onMouseDown={(e) => e.preventDefault()} onClick={onDismiss}>
        <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
      </button>
    </div>
  );
}
