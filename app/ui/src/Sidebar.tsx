import type { CSSProperties, ReactNode } from "react";
import type { DriveSpace, VolumeStatus } from "./types";
import { IClean, IColl, IContract, IGear, ILogo, ISearch, ITime, IMin, ICls } from "./icons";

export type Page = "search" | "timeline" | "collections" | "cleanup" | "settings";

const compact = (n: number) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? Math.round(n / 1e3) + "k" : String(n));
const gb = (n: number) => {
  const g = n / 1024 ** 3;
  return g >= 1000 ? `${(g / 1024).toFixed(g >= 10240 ? 0 : 1)} TB` : g >= 10 ? `${Math.round(g)} GB` : g >= 1 ? `${g.toFixed(1)} GB` : `${Math.max(1, Math.round(n / 1024 ** 2))} MB`;
};

/**
 * v4 left rail of the full window: brand, Search, Library group, the drives card, Settings. Classes: shell.css.
 *
 * The drives card answers two things a person actually wonders about: is search current (one line), and how
 * much room is left on each drive (the bar is the part in use; it turns amber when the drive is nearly full).
 * A click on a drive lists its biggest folders.
 */
export function Sidebar({ page, onPage, status, counts, space, onDrive }: {
  page: Page; onPage: (p: Page) => void; status: VolumeStatus[];
  /** Optional right-aligned counters on the Library items (e.g. "48k", "6", "18.9 GB"). Omitted = none. */
  counts?: Partial<Record<"timeline" | "collections" | "cleanup", string>>;
  /** Capacity per drive; without it a row falls back to how many files are indexed there. */
  space?: DriveSpace[];
  /** Show what takes the space on a drive (its folders, biggest first). */
  onDrive?: (letter: string) => void;
}) {
  const live = status.length > 0 && status.every((v) => v.state === "live");
  const item = (p: Page, label: string, icon: ReactNode, n?: string) => (
    <button key={p} className={"nv" + (page === p ? " on" : "")} onClick={() => onPage(p)} aria-current={page === p ? "page" : undefined} tabIndex={-1}>
      {icon}{label}{n ? <i>{n}</i> : null}
    </button>
  );
  const drives = status.filter((v) => /^[A-Za-z]$/.test(v.letter));
  const files = drives.reduce((a, v) => a + v.files, 0);
  const scanningAny = drives.some((v) => v.state === "scanning");
  const headline = live ? "Search is up to date"
    : status.every((v) => v.state === "demo") && status.length ? "Demo data"
    : status.some((v) => v.state === "paused") ? "Updates paused"
    : status.some((v) => v.state === "offline") ? "Search service is not running"
    : scanningAny ? "Reading your drives…" : status.length ? "Catching up…" : "Connecting…";
  return (
    <aside className="side">
      <div className="brand"><span className="logo"><ILogo /></span>winseek</div>
      {item("search", "Search", <ISearch />)}
      <div className="nlab">Library</div>
      {item("timeline", "Timeline", <ITime />, counts?.timeline)}
      {item("collections", "Collections", <IColl />, counts?.collections)}
      {item("cleanup", "Clean up", <IClean />, counts?.cleanup)}
      <span className="sp" />
      {drives.length > 0 && (
        <div className="idx">
          <div className="h" title={live ? "New, renamed and deleted files show up in search within moments." : undefined}>
            <span className={"dot" + (live ? "" : " warn")} />
            <span>{headline}</span>
          </div>
          {files > 0 && <div className="sub">{compact(files)} files searchable</div>}
          <div className="dl">Free space</div>
          {drives.map((v) => {
            const sp = space?.find((x) => x.letter.toUpperCase() === v.letter.toUpperCase());
            const scan = v.state === "scanning" && v.total ? Math.min(1, (v.done ?? 0) / v.total) : null;
            const used = sp ? 1 - sp.free / sp.total : null;
            const tight = used !== null && used >= 0.9;
            const right = scan !== null ? `Reading ${Math.round(scan * 100)}%` : sp ? gb(sp.free) : `${compact(v.files)} files`;
            const tip = [
              sp ? `${v.letter}: ${gb(sp.free)} free of ${gb(sp.total)}${tight ? " (almost full)" : ""}` : `${v.letter}:`,
              `${v.files.toLocaleString("en-US")} files searchable`,
              onDrive ? "Click to see the biggest folders" : "",
            ].filter(Boolean).join("\n");
            return (
              <button className={"d" + (tight ? " tight" : "")} key={v.letter} tabIndex={-1} title={tip} disabled={!onDrive}
                      aria-label={`Drive ${v.letter}: ${right}${sp ? " free" : ""}. Show its biggest folders.`}
                      onClick={() => onDrive?.(v.letter)}>
                <b>{v.letter}:</b>
                <div className={"meter" + (scan !== null ? " scan" : "")}><i style={{ "--u": scan ?? used ?? 0 } as CSSProperties} /></div>
                <span>{right}</span>
              </button>
            );
          })}
        </div>
      )}
      {item("settings", "Settings", <IGear />)}
    </aside>
  );
}

/** v4 window controls of the full window (minimize / restore to popup / close = hide). */
export function WindowControls({ onMin, onRestore, onClose }: { onMin?: () => void; onRestore: () => void; onClose: () => void }) {
  return (
    <div className="wc">
      {onMin && <button title="Minimize" aria-label="Minimize" tabIndex={-1} onClick={onMin}><IMin /></button>}
      <button title="Back to popup (F11)" aria-label="Back to popup" tabIndex={-1} onClick={onRestore}><IContract /></button>
      <button title="Close" aria-label="Close" tabIndex={-1} onClick={onClose}><ICls /></button>
    </div>
  );
}
