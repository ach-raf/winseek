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

const MAX_SCALE = 32;

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
