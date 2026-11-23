import { memo, type CSSProperties, type ReactNode } from "react";
import type { Backend } from "./types";
import { StreamThumb } from "./StreamThumb";
import { FileIcon } from "./FileIcon";

/** What a tile does when used; `i` is the tile's index in its grid. The functions must be stable (the tile is
 *  memoised: a scroll step re-renders the window, not every tile in it). */
export interface PicTileEvents {
  onClick?(i: number, ev: React.MouseEvent): void;
  onDoubleClick?(i: number, ev: React.MouseEvent): void;
  onContextMenu?(i: number, ev: React.MouseEvent): void;
  onPointerDown?(i: number, ev: React.PointerEvent): void;
}

/**
 * The one picture tile (search Pictures, Timeline, Collections): an exact-size 4:3 box with the streamed picture
 * (StreamThumb: what the disk already has at once, then the picture decoded at the tile's size), a caption that fades in on hover / selection, an optional badge, and the
 * shared selection ring. `tileW` x `tileH` is the box (gridMetrics); `style` adds the position for absolute grids.
 */
export const PicTile = memo(function PicTile({ i, path, name, mtime, tileW, tileH, sel = false, lead = false, named = false, doc = false, badge, caption, title, style, ev, label }: {
  backend: Backend;
  i: number;
  path: string;
  name: string;
  mtime?: number | null;
  tileW: number;
  tileH: number;
  sel?: boolean;
  /** The keyboard's item inside a multi-selection. */
  lead?: boolean;
  /** There for its name: the caption stays on. */
  named?: boolean;
  /** Not a picture or video: the name stays visible (a wall of anonymous document icons is unreadable). */
  doc?: boolean;
  badge?: ReactNode;
  /** The caption's content (defaults to the name). */
  caption?: ReactNode;
  title?: string;
  style?: CSSProperties;
  ev: PicTileEvents;
  label?: string;
}) {
  const ph = <span className="pt-ph"><FileIcon name={name} isDir={false} src={null} /></span>;
  return (
    <div
      role="option"
      aria-selected={sel}
      aria-label={label ?? name}
      className={"pt" + (sel ? " sel" : "") + (lead ? " lead" : "") + (named ? " named" : "") + (doc ? " doc" : "")}
      style={{ width: tileW, height: tileH, ...style }}
      data-i={i}
      data-sel={sel ? "1" : undefined}
      title={title}
      onClick={ev.onClick && ((e) => ev.onClick!(i, e))}
      onDoubleClick={ev.onDoubleClick && ((e) => ev.onDoubleClick!(i, e))}
      onContextMenu={ev.onContextMenu && ((e) => ev.onContextMenu!(i, e))}
      onPointerDown={ev.onPointerDown && ((e) => ev.onPointerDown!(i, e))}
    >
      <StreamThumb path={path} mtime={mtime} tileW={tileW} tileH={tileH} bad={ph} />
      {badge}
      <span className="pt-cap" dir="auto">{caption ?? name}</span>
    </div>
  );
});
