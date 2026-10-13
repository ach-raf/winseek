import { memo } from "react";
import type { SemanticHit } from "./types";
import { FileIcon } from "./FileIcon";
import { kindOf, splitDir } from "./format";

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

/** Subtle "why it matched" text: image hits are matched visually, so say what they look like. */
export function why(path: string, query: string): string {
  return kindOf(baseName(path), false) === "image" ? `looks like: ${query}` : `matches: ${query}`;
}

export const SemRow = memo(function SemRow({ hit, query, selected, icon, thumb, onPick, onHover, onMenu }: {
  hit: SemanticHit; query: string; selected: boolean; icon: string | null; thumb: boolean;
  onPick: () => void; onHover: () => void; onMenu: (e: React.MouseEvent) => void;
}) {
  const name = baseName(hit.path);
  const { head, tail } = splitDir(hit.path);
  return (
    <div className={"row flow" + (selected ? " selected" : "")} role="option" aria-selected={selected}
         data-sel={selected ? "1" : undefined} title={hit.path}
         onClick={onPick} onMouseMove={onHover} onContextMenu={onMenu}>
      <FileIcon name={name} isDir={false} src={icon} thumb={thumb} />
      <div className="meta">
        <div className="name">{name}</div>
        <div className="path"><span className="head">{head}</span><span className="tail">{tail}</span></div>
      </div>
      <div className="side why" title={`Similarity score ${hit.score.toFixed(3)}`}>
        <span className="why-text">{why(hit.path, query)}</span>
        <span className="why-score">score {hit.score.toFixed(2)}</span>
      </div>
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
