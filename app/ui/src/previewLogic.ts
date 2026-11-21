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

/** A position in a recording: "0:07", "1:23", "1:02:03". */
export function formatClock(s: number): string {
  const t = Number.isFinite(s) ? Math.max(0, Math.floor(s)) : 0;
  const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), sec = t % 60;
  const ss = String(sec).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/**
 * Where a video / audio element should start for a hit anchored at `start` seconds: null when the hit has no
 * (usable) anchor, else the anchor kept inside the clip (an unknown duration leaves it as is).
 */
export function seekTarget(start: number | null | undefined, duration: number): number | null {
  if (start == null || !Number.isFinite(start) || start <= 0) return null;
  return Number.isFinite(duration) && duration > 0 ? Math.min(start, duration) : start;
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

/** Find the query terms in `text` as [start, end) offsets — case-insensitive, longest term wins
 *  at each position, at most `cap` matches. The one matcher every view paints from: the plain
 *  text view turns the offsets straight into Custom Highlight ranges over its single text node,
 *  the rich views (markdown AST, Prism tokens) split their nodes on them through highlight(). */
export function matchRanges(text: string, terms: string[], cap = 2000): { ranges: Array<[number, number]>; hits: number } {
  if (!terms.length || !text) return { ranges: [], hits: 0 };
  const re = new RegExp(
    terms.slice().sort((a, b) => b.length - a.length).map(escapeRe).join("|"),
    "gi",
  );
  const ranges: Array<[number, number]> = [];
  for (let m = re.exec(text); m && ranges.length < cap; m = re.exec(text)) {
    if (!m[0]) { re.lastIndex++; continue; }
    ranges.push([m.index, m.index + m[0].length]);
  }
  return { ranges, hits: ranges.length };
}

/** Split `text` into plain / matching segments (case-insensitive). At most `cap` matches are marked. */
export function highlight(text: string, terms: string[], cap = 2000): { segments: Segment[]; hits: number } {
  const { ranges, hits } = matchRanges(text, terms, cap);
  if (ranges.length === 0) return { segments: [{ text, hit: false }], hits: 0 };
  const segments: Segment[] = [];
  let at = 0;
  for (const [s, e] of ranges) {
    if (s > at) segments.push({ text: text.slice(at, s), hit: false });
    segments.push({ text: text.slice(s, e), hit: true });
    at = e;
  }
  if (at < text.length) segments.push({ text: text.slice(at), hit: false });
  return { segments, hits };
}

// ---- rich text & code preview ---------------------------------------------------------------------------
// What a file preview shows: formatted Markdown ("Preview" view) or Prism-highlighted source.
// Pure data only — the React views in Preview.tsx wire these into the libraries.

/** Extensions the preview renders as formatted Markdown instead of source text. */
const MD_EXTS = new Set(["md", "markdown", "mdown", "mkd"]);

/** True when a file of this extension reads as Markdown in the preview. */
export function isMarkdown(ext: string): boolean {
  return MD_EXTS.has(ext.toLowerCase());
}

/** File extension / fence language name -> Prism language id for the highlighter (the set
 *  prism-react-renderer vendors plus the prismjs components registered in prismLangs.ts).
 *  Canonical language names are keys too, so Markdown fence info strings resolve directly. */
const LANG_BY_EXT: Record<string, string> = {
  ts: "typescript", mts: "typescript", cts: "typescript", tsx: "tsx", typescript: "typescript",
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "jsx", javascript: "javascript",
  json: "json", jsonc: "json",
  html: "markup", htm: "markup", xml: "markup", svg: "markup", xhtml: "markup", vue: "markup",
  csproj: "markup", vbproj: "markup", resx: "markup",
  css: "css", scss: "scss", less: "less",
  md: "markdown", markdown: "markdown",
  py: "python", pyw: "python", python: "python",
  rs: "rust", rust: "rust",
  go: "go", golang: "go",
  c: "c", h: "c",
  cpp: "cpp", cc: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", hxx: "cpp",
  cs: "csharp", csharp: "csharp",
  java: "java",
  rb: "ruby", ruby: "ruby",
  sql: "sql",
  sh: "bash", bash: "bash", zsh: "bash",
  ps1: "powershell", psm1: "powershell", psd1: "powershell", powershell: "powershell",
  bat: "batch", cmd: "batch", batch: "batch",
  yaml: "yaml", yml: "yaml",
  toml: "toml",
  ini: "ini", cfg: "ini", conf: "ini", properties: "ini",
  diff: "diff", patch: "diff",
  swift: "swift",
  kt: "kotlin", kts: "kotlin", kotlin: "kotlin",
  graphql: "graphql", gql: "graphql",
};

/** The Prism language that fits a file extension, or null to show the file as plain text. */
export function codeLanguage(ext: string): string | null {
  return LANG_BY_EXT[ext.toLowerCase()] ?? null;
}

/** Markdown fence info string -> Prism language id (a few aliases the map above misses). */
export function fenceLanguage(info: string): string | null {
  const lang = info.toLowerCase().trim();
  if (LANG_BY_EXT[lang]) return LANG_BY_EXT[lang]!;
  const alias: Record<string, string> = {
    shell: "bash", console: "bash", terminal: "bash", "c++": "cpp", objc: "objectivec",
  };
  return alias[lang] ?? null; // an unknown fence renders as an unstyled block
}

/** Pretty-print JSON for the preview. Null when the content is not JSON worth re-indenting
 *  (unparseable text, or a bare scalar): then the raw text is the right preview. */
export function prettyJson(content: string): string | null {
  try {
    const value: unknown = JSON.parse(content);
    return value !== null && typeof value === "object" ? JSON.stringify(value, null, 2) : null;
  } catch {
    return null; // not JSON: the raw text stands (this is the expected path for any other file)
  }
}

/** Extensions the pane renders as real Word documents (docx-preview over the streamed bytes). */
const WORD_EXTS = new Set(["docx", "docm", "dotx", "dotm"]);

/** True when a file of this extension can render as a Word document in the preview. */
export function isWordDoc(ext: string): boolean {
  return WORD_EXTS.has(ext.toLowerCase());
}

/** The selected text when the live selection starts inside `root` (the preview pane), else "".
 *  Focus usually sits in the search box, so a native Ctrl+C copies nothing: both the Ctrl+C
 *  branch and the preview's right-click menu use this to mean "the user means this text". */
export function previewSelectionText(sel: { anchorNode: unknown; toString(): string } | null, root: Element | null): string {
  if (!sel || !root) return "";
  const text = sel.toString();
  if (!text) return "";
  const at = sel.anchorNode;
  return typeof at === "object" && at !== null && root.contains(at as Node) ? text : "";
}

/** A hast node as react-markdown hands it over — just the fields the term-marker touches.
 *  Structural on purpose: no library types, so `node --test` can load this file. */
export interface MdNode {
  type: string;
  value?: string;
  tagName?: string;
  properties?: Record<string, unknown>;
  children?: MdNode[];
}

/** Mark the query terms inside a rendered-Markdown tree, in place: every text node holding a
 *  term is split into plain / `mark` element children — the same segments the source views use,
 *  so one highlighter decides what a hit is everywhere. */
export function markTextNodes(node: MdNode, terms: string[]): void {
  if (terms.length === 0 || !node.children) return;
  for (let i = 0; i < node.children.length; i++) {
    const child = node.children[i]!;
    if (child.type === "text" && typeof child.value === "string") {
      const lower = child.value.toLowerCase();
      if (!terms.some((t) => lower.includes(t.toLowerCase()))) continue;
      const parts = highlight(child.value, terms).segments;
      if (!parts.some((p) => p.hit)) continue;
      node.children.splice(i, 1, ...parts.map((p) => p.hit
        ? { type: "element", tagName: "mark", properties: {}, children: [{ type: "text", value: p.text }] }
        : { type: "text", value: p.text }));
      i += parts.length - 1;
    } else {
      markTextNodes(child, terms);
    }
  }
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
  return m ? m[1]!.toUpperCase() : "";
}
