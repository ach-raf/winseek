import { useState, useEffect, type CSSProperties } from "react";
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
    if (ok) return <AppPlate src={src!} onFail={() => setFailed(src!)} />;
    const [a, b] = appGradient(name);
    return <span className="ic ficon app" aria-hidden style={{ "--a": a, "--b": b } as CSSProperties}>{initials(name)}</span>;
  }
  if (isDir) return <span className="ic ficon" aria-hidden><IFolder /></span>;
  if (img && thumb) return <span className="ic ficon ph" aria-hidden>{img}</span>;
  if (img) return <span className="ic ficon sicon" aria-hidden>{img}</span>;
  const g = glyphFor(name);
  return <span className="ic ficon" aria-hidden><FileGlyph col={g.col} lab={g.lab} /></span>;
}


/* ---- iOS-style app plate ----
 * Apple makes every icon read as one family by masking all of them with the same continuous-corner
 * squircle. Full-bleed artwork is just clipped to it; a glyph on transparency (most Windows icons) is
 * set on a plate tinted from its own colour, at ~68 % size, so nothing floats on the page or clashes. */
type Plate = { kind: "bleed" } | { kind: "tile"; a: string; b: string };
const plates = new Map<string, Plate>();

function analyse(img: HTMLImageElement): Plate | null {
  try {
    const N = 32, cv = document.createElement("canvas"); cv.width = cv.height = N;
    const cx = cv.getContext("2d", { willReadFrequently: true }); if (!cx) return null;
    cx.drawImage(img, 0, 0, N, N);
    const d = cx.getImageData(0, 0, N, N).data;
    let n = 0, r = 0, g = 0, b = 0, w = 0;
    for (let i = 0; i < d.length; i += 4) {
      const al = d[i + 3]!; if (al < 40) continue;
      n++; const mx = Math.max(d[i]!, d[i + 1]!, d[i + 2]!), mn = Math.min(d[i]!, d[i + 1]!, d[i + 2]!);
      const wt = 0.15 + (mx - mn) / 255; // saturated pixels carry the icon's identity
      r += d[i]! * wt; g += d[i + 1]! * wt; b += d[i + 2]! * wt; w += wt;
    }
    if (!n) return null;
    const corner = (x: number, y: number) => d[(y * N + x) * 4 + 3]!;
    if (n / (N * N) > 0.9 && corner(1, 1) > 240 && corner(N - 2, 1) > 240 && corner(1, N - 2) > 240 && corner(N - 2, N - 2) > 240) return { kind: "bleed" };
    r /= w; g /= w; b /= w;
    const mx = Math.max(r, g, b) / 255, mn = Math.min(r, g, b) / 255, l = (mx + mn) / 2;
    const sat = mx === mn ? 0 : (mx - mn) / (1 - Math.abs(2 * l - 1));
    let h = 0; const dd = mx - mn;
    if (dd) { const R = r / 255, G = g / 255; h = mx === R ? ((G - b / 255) / dd) % 6 : mx === G ? (b / 255 - R) / dd + 2 : (R - G) / dd + 4; h = (h * 60 + 360) % 360; }
    const dark = document.documentElement.dataset.theme !== "light";
    const s = Math.round(Math.min(sat, 1) * 45);
    if (l < 0.28) return dark ? { kind: "tile", a: "#e9ebf0", b: "#c9cdd6" } : { kind: "tile", a: "#fafafc", b: "#e2e5eb" }; // dark glyph -> light plate
    if (sat < 0.12) return dark ? { kind: "tile", a: "#33353d", b: "#1d1e23" } : { kind: "tile", a: "#fdfdfe", b: "#e6e9ef" };
    return dark ? { kind: "tile", a: `hsl(${h} ${s}% 30%)`, b: `hsl(${h} ${s}% 17%)` } : { kind: "tile", a: `hsl(${h} ${s + 10}% 97%)`, b: `hsl(${h} ${s + 10}% 86%)` };
  } catch { return null; }
}

function AppPlate({ src, onFail }: { src: string; onFail: () => void }) {
  const [plate, setPlate] = useState<Plate | null | undefined>(() => plates.get(src));
  useEffect(() => { setPlate(plates.get(src)); }, [src]);
  const onLoad = (e: React.SyntheticEvent<HTMLImageElement>) => {
    if (plates.has(src)) return;
    const p = analyse(e.currentTarget);
    if (p) {
      // Cache cap: drop the single oldest plate (a Map iterates insertion order). Wiping everything
      // instead re-analysed every visible icon at once — a mid-scroll latency spike.
      if (plates.size >= 800) {
        const oldest = plates.keys().next().value;
        if (oldest !== undefined) plates.delete(oldest);
      }
      plates.set(src, p); setPlate(p);
    }
  };
  const bleed = plate?.kind === "bleed";
  const style = plate?.kind === "tile" ? ({ "--pa": plate.a, "--pb": plate.b } as CSSProperties) : undefined;
  return (
    <span className={"ic ficon plate" + (bleed ? " bleed" : "")} style={style} aria-hidden>
      <img src={src} crossOrigin="anonymous" loading="lazy" draggable={false} alt="" onLoad={onLoad} onError={onFail} />
    </span>
  );
}
