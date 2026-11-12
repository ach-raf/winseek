// Pure helpers for the Space preview (no imports, so `node --test` can load this file).

/** Words of the query worth highlighting in a file: no operators (`ext:`, `size:`), negations or wildcards. */
export function previewTerms(query: string): string[] {
  const out: string[] = [];
  for (const raw of query.split(/\s+/)) {
    if (!raw || raw.startsWith("!")) continue;
    const t = raw.replace(/^"+|"+$/g, "").replace(/[*?]/g, "");
    if (t.length < 2 || t.includes(":")) continue;
    if (!out.some((x) => x.toLowerCase() === t.toLowerCase())) out.push(t);
  }
  return out;
}

/** Keep a PDF page number inside 1..pages (unknown count: at least 1). */
export function clampPage(page: number, pages: number | null): number {
  const p = Number.isFinite(page) ? Math.floor(page) : 1;
  const max = pages != null && pages > 0 ? pages : Number.MAX_SAFE_INTEGER;
  return Math.min(Math.max(1, p), max);
}

/** Pixel width to ask pdfium for: the pane width at device resolution, rounded up to 100 px (cache friendly), 400..1600. */
export function pdfRenderWidth(boxWidth: number, dpr: number): number {
  const raw = Math.max(0, boxWidth) * Math.max(1, dpr);
  return Math.min(1600, Math.max(400, Math.ceil(raw / 100) * 100));
}

export interface Segment {
  text: string;
  hit: boolean;
}

const escapeRe = (t: string) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Split `text` into plain / matching segments (case-insensitive). At most `cap` matches are marked. */
export function highlight(text: string, terms: string[], cap = 2000): { segments: Segment[]; hits: number } {
  if (!terms.length || !text) return { segments: [{ text, hit: false }], hits: 0 };
  const re = new RegExp(
    terms.slice().sort((a, b) => b.length - a.length).map(escapeRe).join("|"),
    "gi",
  );
  const segments: Segment[] = [];
  let at = 0, hits = 0;
  for (let m = re.exec(text); m && hits < cap; m = re.exec(text)) {
    if (!m[0]) { re.lastIndex++; continue; }
    if (m.index > at) segments.push({ text: text.slice(at, m.index), hit: false });
    segments.push({ text: m[0], hit: true });
    at = m.index + m[0].length;
    hits++;
  }
  if (at < text.length) segments.push({ text: text.slice(at), hit: false });
  return { segments, hits };
}

// ---- image zoom / pan -----------------------------------------------------------------------------------
// The image is centred in the stage; `t` is its offset from the centre in stage pixels, `s` its scale.

export interface View {
  s: number;
  tx: number;
  ty: number;
}

/** Scale at which the whole image fits the stage (never enlarges beyond 1:1). */
export function fitScale(cw: number, ch: number, iw: number, ih: number): number {
  if (iw <= 0 || ih <= 0 || cw <= 0 || ch <= 0) return 1;
  return Math.min(1, cw / iw, ch / ih);
}

export const MAX_SCALE = 32;

/** Keep the image from being dragged out of the stage: no gap on an axis where it is larger than the stage. */
export function clampView(v: View, cw: number, ch: number, iw: number, ih: number): View {
  const lim = (img: number, box: number) => Math.max(0, (img * v.s - box) / 2);
  const lx = lim(iw, cw), ly = lim(ih, ch);
  return { s: v.s, tx: Math.max(-lx, Math.min(lx, v.tx)), ty: Math.max(-ly, Math.min(ly, v.ty)) };
}

/** Zoom by `factor` keeping the image point under (px, py) (stage pixels from the stage centre) in place. */
export function zoomAt(v: View, factor: number, px: number, py: number, fit: number, cw: number, ch: number, iw: number, ih: number): View {
  const s = Math.max(fit, Math.min(MAX_SCALE, v.s * factor));
  const k = s / v.s;
  return clampView({ s, tx: px - (px - v.tx) * k, ty: py - (py - v.ty) * k }, cw, ch, iw, ih);
}

/** Double-click: 1:1 when fitted, back to fit otherwise. */
export function toggleActual(v: View, fit: number): View {
  return Math.abs(v.s - 1) < 0.005 || fit >= 1 ? { s: fit, tx: 0, ty: 0 } : { s: 1, tx: 0, ty: 0 };
}

// ---- search-hit boxes ---------------------------------------------------------------------------------------
// The backend sends where the query words sit as fractions 0-1 of the image / PDF page.

export interface HitBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
const round6 = (n: number) => Math.round(n * 1e6) / 1e6;

/** Usable boxes only: finite numbers, clamped into the image, and at least `min` (a fraction) wide and high. */
export function cleanBoxes(boxes: readonly HitBox[] | null | undefined, min = 0.004): HitBox[] {
  const out: HitBox[] = [];
  for (const b of boxes ?? []) {
    if (![b.x, b.y, b.w, b.h].every(Number.isFinite)) continue;
    const x = clamp01(b.x), y = clamp01(b.y);
    const w = Math.min(clamp01(b.x + b.w) - x, 1 - x), h = Math.min(clamp01(b.y + b.h) - y, 1 - y);
    if (w <= 0 || h <= 0) continue;
    out.push({ x: round6(x), y: round6(y), w: round6(Math.max(w, min)), h: round6(Math.max(h, min)) });
  }
  return out;
}

const pct = (n: number) => `${Math.round(n * 1e4) / 100}%`;

/** CSS percentages for a box, grown by `pad` (a fraction of the image) so the highlight does not hug the glyphs. */
export function boxRect(b: HitBox, pad = 0.003): { left: string; top: string; width: string; height: string } {
  const x = clamp01(b.x - pad), y = clamp01(b.y - pad);
  return { left: pct(x), top: pct(y), width: pct(clamp01(b.x + b.w + pad) - x), height: pct(clamp01(b.y + b.h + pad) - y) };
}

/**
 * The boxes to draw. `viewPage` null = an image: always. A PDF page (`viewPage` set) only shows them when it is the
 * page the hit is on.
 */
export function boxesForView(boxes: readonly HitBox[] | null | undefined, hitPage: number | null | undefined, viewPage: number | null): HitBox[] {
  if (viewPage !== null && (hitPage == null || hitPage !== viewPage)) return [];
  return cleanBoxes(boxes);
}

/** "Table 2: 3 rows × 2 columns" for the picker shown when a page holds several tables (`tsv` = tab-separated rows). */
export function tableLabel(index: number, tsv: string): string {
  const rows = tsv ? tsv.split("\n") : [];
  const cols = rows.reduce((m, r) => Math.max(m, r.split("\t").length), 0);
  const s = (n: number) => (n === 1 ? "" : "s");
  return rows.length ? `Table ${index + 1}: ${rows.length} row${s(rows.length)} × ${cols} column${s(cols)}` : `Table ${index + 1}`;
}

// ---- pane header: kind label, breadcrumbs, drive -----------------------------------------------------------

const KIND_WORD: Record<string, string> = {
  image: "Image", video: "Video", audio: "Audio", pdf: "PDF document", document: "Document", text: "Text file", folder: "Folder",
};

/** Kinds whose generic word would hide the actual type (a wall of "Image" says nothing about PNG vs JPG). */
const KIND_PREFIX_EXT = new Set(["image", "video", "audio"]);

/** "PDF document", "PNG image", "Folder", "ZIP file" ... Falls back to the extension, then "File". */
export function kindLabel(kind: string | null | undefined, ext: string, isDir: boolean): string {
  if (isDir || kind === "folder") return "Folder";
  const word = kind && KIND_WORD[kind];
  const e = ext.replace(/^\./, "").trim();
  if (word && !(KIND_PREFIX_EXT.has(kind!) && e)) return word;
  if (word) return `${e.toUpperCase()} ${word.toLowerCase()}`;
  return e ? `${e.toUpperCase()} file` : "File";
}

/** Extension of a file name without the dot ("" when none). */
export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i > 0 && i < name.length - 1 ? name.slice(i + 1) : "";
}

/**
 * Breadcrumb parts of the folder holding a hit (the folder itself for a folder hit). The home folder prefix
 * (`C:\Users\name`) is dropped; a hit directly in the home folder yields ["Home"].
 */
export function crumbParts(path: string, isDir: boolean): string[] {
  let p = path.replace(/[\\/]+$/, "");
  if (!isDir) {
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    p = i >= 0 ? p.slice(0, i) : "";
  }
  const home = p.match(/^[A-Za-z]:[\\/]+Users[\\/]+[^\\/]+/i);
  if (home) {
    const rest = p.slice(home[0].length);
    const parts = rest.split(/[\\/]+/).filter(Boolean);
    return parts.length ? parts : ["Home"];
  }
  const parts = p.split(/[\\/]+/).filter(Boolean);
  return parts.length ? parts : [path];
}

/** "C:" for "C:\a\b", "" when the path has no drive letter. */
export function driveOf(path: string): string {
  const m = path.match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() : "";
}
