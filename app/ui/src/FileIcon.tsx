import { useState, type CSSProperties } from "react";
import { FileGlyph, ICmd, IFolder, ISet } from "./icons";
import { appGradient, glyphFor, initials } from "./iconLogic";

/**
 * v4 result icon: a 36 px `.ic` box (size set by its context: row 36, top result 52, app tile 46).
 * Files: a thumbnail (`thumb`), the shell icon when the backend has one, else the paper sheet with a coloured
 * extension badge. Folders: the folder. Apps: the real icon, else a gradient squircle with initials (stable per
 * name). Settings / commands: the grey squircle glyphs.
 */
export function FileIcon({ name, isDir, src, thumb, variant }: {
  name: string; isDir: boolean; src?: string | null; thumb?: boolean; variant?: "app" | "setting" | "command";
}) {
  const [failed, setFailed] = useState<string | null>(null);
  const ok = !!src && failed !== src;
  const img = ok ? <img src={src!} loading="lazy" draggable={false} alt="" onError={() => setFailed(src!)} /> : null;

  if (variant === "setting") return <span className="ic ficon sys" aria-hidden><ISet /></span>;
  if (variant === "command") return <span className="ic ficon sys cmd" aria-hidden><ICmd /></span>;
  if (variant === "app") {
    if (img) return <span className="ic ficon shell" aria-hidden>{img}</span>;
    const [a, b] = appGradient(name);
    return <span className="ic ficon app" aria-hidden style={{ "--a": a, "--b": b } as CSSProperties}>{initials(name)}</span>;
  }
  if (isDir) return <span className="ic ficon" aria-hidden><IFolder /></span>;
  if (img && thumb) return <span className="ic ficon ph" aria-hidden>{img}</span>;
  if (img) return <span className="ic ficon shell" aria-hidden>{img}</span>;
  const g = glyphFor(name);
  return <span className="ic ficon" aria-hidden><FileGlyph col={g.col} lab={g.lab} /></span>;
}
