import type { CSSProperties, ReactNode } from "react";
import type { VolumeStatus } from "./types";
import { IClean, IColl, IGear, ILogo, ISearch, ITime, IMin, IMax, ICls } from "./icons";

export type Page = "search" | "timeline" | "collections" | "cleanup" | "settings";

const compact = (n: number) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? Math.round(n / 1e3) + "k" : String(n));

/** v4 left rail of the full window: brand, Search, Library group, index meters, Settings. Classes: shell.css. */
export function Sidebar({ page, onPage, status, counts }: {
  page: Page; onPage: (p: Page) => void; status: VolumeStatus[];
  /** Optional right-aligned counters on the Library items (e.g. "48k", "6", "18.9 GB"). Omitted = none. */
  counts?: Partial<Record<"timeline" | "collections" | "cleanup", string>>;
}) {
  const live = status.length > 0 && status.every((v) => v.state === "live");
  const max = Math.max(1, ...status.map((v) => v.files + v.folders));
  const item = (p: Page, label: string, icon: ReactNode, n?: string) => (
    <button key={p} className={"nv" + (page === p ? " on" : "")} onClick={() => onPage(p)} aria-current={page === p ? "page" : undefined} tabIndex={-1}>
      {icon}{label}{n ? <i>{n}</i> : null}
    </button>
  );
  const headline = live ? "Index up to date"
    : status.every((v) => v.state === "demo") && status.length ? "Demo data"
    : status.length ? "Indexing…" : "Connecting…";
  return (
    <aside className="side">
      <div className="brand"><span className="logo"><ILogo /></span>winseek</div>
      {item("search", "Search", <ISearch />)}
      <div className="nlab">Library</div>
      {item("timeline", "Timeline", <ITime />, counts?.timeline)}
      {item("collections", "Collections", <IColl />, counts?.collections)}
      {item("cleanup", "Clean up", <IClean />, counts?.cleanup)}
      <span className="sp" />
      <div className="idx">
        <div className="h"><span className={"dot" + (live ? "" : " warn")} />{headline}</div>
        {status.map((v) => {
          const n = v.files + v.folders;
          const scanning = v.state === "scanning" && v.total ? Math.min(1, (v.done ?? 0) / v.total) : null;
          return (
            <div className="d" key={v.letter} title={`${n.toLocaleString("en-US")} entries · ${v.state}`}>
              <b>{v.letter}:</b>
              <div className="meter"><i style={{ "--u": scanning ?? n / max } as CSSProperties} /></div>
              <span>{scanning !== null ? Math.round(scanning * 100) + "%" : compact(n)}</span>
            </div>
          );
        })}
      </div>
      {item("settings", "Settings", <IGear />)}
    </aside>
  );
}

/** v4 window controls of the full window (minimize / restore to popup / close = hide). */
export function WindowControls({ onMin, onRestore, onClose }: { onMin?: () => void; onRestore: () => void; onClose: () => void }) {
  return (
    <div className="wc">
      {onMin && <button title="Minimize" aria-label="Minimize" tabIndex={-1} onClick={onMin}><IMin /></button>}
      <button title="Back to popup (F11)" aria-label="Back to popup" tabIndex={-1} onClick={onRestore}><IMax /></button>
      <button title="Close" aria-label="Close" tabIndex={-1} onClick={onClose}><ICls /></button>
    </div>
  );
}
