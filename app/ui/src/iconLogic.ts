// Pure helpers behind the v4 file / app icons (no imports, so `node --test` can load it).

/** Badge colours of the file glyph (paper sheet + coloured extension badge). */
export const FCOL = {
  pdf: "#e5484d", doc: "#2f6fed", xls: "#1f9d55", ppt: "#e8590c", txt: "#6b7280", code: "#475569", zip: "#b7791f", aud: "#d6409f", vid: "#7c5ce0",
} as const;
const OTHER = "#6b7280";

const GROUPS: [keyof typeof FCOL, string[]][] = [
  ["pdf", ["pdf"]],
  ["doc", ["doc", "docx", "odt", "rtf", "pages"]],
  ["xls", ["xls", "xlsx", "csv", "ods", "numbers"]],
  ["ppt", ["ppt", "pptx", "odp", "key"]],
  ["txt", ["txt", "md", "log", "text"]],
  ["code", ["rs", "py", "ts", "tsx", "js", "jsx", "json", "toml", "c", "cpp", "h", "hpp", "cs", "java", "go", "rb", "php", "sh", "ps1", "bat", "html", "css", "yml", "yaml", "xml", "sql", "lua", "kt", "swift"]],
  ["zip", ["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "iso"]],
  ["aud", ["mp3", "flac", "wav", "m4a", "ogg", "aac", "opus", "wma"]],
  ["vid", ["mp4", "mkv", "mov", "avi", "webm", "wmv", "m4v"]],
];
const EXT_GROUP = new Map<string, keyof typeof FCOL>(GROUPS.flatMap(([g, list]) => list.map((e) => [e, g] as const)));

/** Extension badge of a file name: up to 4 upper-case letters, "" when there is no extension. */
export function extLabel(name: string): string {
  const d = name.lastIndexOf(".");
  return d > 0 && d < name.length - 1 ? name.slice(d + 1).toUpperCase().slice(0, 4) : "";
}

/** Colour + label of the file glyph for a file name. */
export function glyphFor(name: string): { col: string; lab: string } {
  const lab = extLabel(name);
  const g = EXT_GROUP.get(lab.toLowerCase());
  return { col: g ? FCOL[g] : OTHER, lab };
}

const hash = (s: string): number => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
};

/** Stable gradient for an app tile, derived from the name: [light, dark]. */
export function appGradient(name: string): [string, string] {
  const hue = hash(name.toLowerCase()) % 360;
  return [`hsl(${hue} 72% 60%)`, `hsl(${(hue + 28) % 360} 70% 40%)`];
}

/** One or two letters for an app tile: "Visual Studio Code" -> "VS", "Figma" -> "Fi", "mpv" -> "Mp". */
export function initials(name: string): string {
  const words = name.split(/[^A-Za-z0-9]+/).filter(Boolean);
  if (!words.length) return name.trim().slice(0, 1).toUpperCase() || "?";
  if (words.length > 1) return (words[0][0] + words[1][0]).toUpperCase();
  const w = words[0];
  return w.length > 1 ? w[0].toUpperCase() + w[1].toLowerCase() : w.toUpperCase();
}
