// Path clean-up for pasted search text. Pure and import-free so `node --test` can load it
// directly (Run: npm test).
// A path copied out of JSON ("E:\\a\\b.winmd"), from an ffmpeg line (-i "C:/v/clip.mp4"),
// or as a file:///E:/a%20b.mp4 URL should find the file instead of being searched literally.

/** The best file-path candidate inside pasted text, or null when there is none. */
export function extractPath(text: string): string | null {
  const t = text.trim();
  if (!t) return null;
  const cands: string[] = [];
  // a whole single line starting with a drive letter is one path, spaces included
  if (/^[A-Za-z]:[\\/][^\n]+$/.test(t)) cands.push(t);
  // quoted runs (JSON values, ffmpeg args) whose inside carries a drive/UNC/file:// marker
  for (const m of t.match(/"([^"\n]+)"|'([^'\n]+)'/g) ?? []) {
    const inner = m.slice(1, -1);
    if (/\b[A-Za-z]:[\\/]|\\\\|^file:\/\//i.test(inner)) cands.push(inner);
  }
  // bare candidates: drive path, file URL, UNC — stop at whitespace/quote/comma/angle
  for (const m of t.match(/[A-Za-z]:[\\/][^\s"',;<>]*|file:\/\/\/[^\s"',;<>]*|\\\\[^\s"',;<>]+/g) ?? []) {
    cands.push(m);
  }
  if (cands.length === 0) return null;
  // longest wins: E:\a\b\c.mp4 beats the E:\a\b fragment
  return cands.reduce((a, b) => (b.length > a.length ? b : a));
}

/** Normalize one path to the single-backslash form the index stores. */
export function normalizeWinPath(raw: string): string {
  let p = raw.trim();
  const lower = p.toLowerCase();
  if (lower.startsWith("file:///")) p = p.slice(8);
  else if (lower.startsWith("file://")) p = p.slice(7);
  if (lower.startsWith("file://")) p = percentDecode(p);
  // one wrapping quote pair ('...' / "...") glued on by console lines
  if (p.length >= 2) {
    const f = p[0];
    if ((f === '"' || f === "'") && f === p[p.length - 1]) p = p.slice(1, -1);
  }
  // runs of \ and / (doubled JSON escapes, forward slashes) become one backslash
  p = p.replace(/[\\/]+/g, "\\");
  // trailing copy junk; a leading separator is kept (\\server shares)
  return p.replace(/[",;]+$/, "").trim();
}

/** From pasted/hotkey text to query text: the extracted path, quoted when it has spaces so
 *  the tokenizer keeps it one term — null when the text holds no path (left as-is).
 *  Search syntax typed on purpose (wildcards, negation, operators) disables extraction:
 *  that paste already IS a query. */
export function pathQueryFromPaste(text: string): string | null {
  const t = text.trim();
  if (!t || /[*?!]|(?:^|\s)(?:ext|folder|size|dm|ws):|(?:^|\s)file:(?!\/\/)/i.test(t)) return null;
  const path = normalizeWinPath(extractPath(t) ?? "");
  if (!path) return null;
  return /\s/.test(path) ? `"${path}"` : path;
}

function percentDecode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    // stray %: decode only the well-formed pairs, keep everything else literal
  }
  return s.replace(/%([0-9A-Fa-f]{2})/g, (_m, h) => String.fromCharCode(parseInt(h, 16)));
}
