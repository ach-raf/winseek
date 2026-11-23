import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { fitMenuRect } from "./popupLogic";
import type { LaunchHit } from "./types";

/** The launcher context menu (PRD B5): full keyboard support — the first item is focused on
 *  open, ↑ ↓ (wrap) Home End move, Enter/Space activate, and the underlined access letters
 *  (Open O, admin A, location L, Copy C, Remove R, Hide H) jump straight to the action. */
export function LaunchMenu({ menu, idx, setIdx, listRef, onRun, onRemove, onHide, onClose }: {
  menu: { hit: LaunchHit; x: number; y: number };
  idx: number;
  setIdx: (i: number) => void;
  listRef: RefObject<HTMLDivElement | null>;
  onRun: (mode: "open" | "admin" | "reveal" | "copy") => void;
  /** Only on a "Most used" tile: drop the entry and its weight from the home history
   *  (not a results hide — launching the app again brings it back). */
  onRemove?: () => void;
  onHide: () => void;
  onClose: () => void;
}) {
  const kind = menu.hit.item.kind;
  const items = [
    { id: "open", label: "Open", acc: "o", run: () => onRun("open") },
    ...(kind === "app" && menu.hit.item.admin_ok ? [{ id: "admin", label: "Run as administrator", acc: "a", run: () => onRun("admin") }] : []),
    ...(kind === "app" ? [{ id: "reveal", label: "Open file location", acc: "l", run: () => onRun("reveal") }] : []),
    ...(kind === "setting" ? [{ id: "copy", label: "Copy link", acc: "c", run: () => onRun("copy") }] : []),
    ...(kind === "app" ? [{ id: "copy", label: "Copy target", acc: "c", run: () => onRun("copy") }] : []),
    ...(onRemove ? [{ id: "remove", label: "Remove from this list", acc: "r", run: onRemove }] : []),
    { id: "hide", label: "Hide from results", acc: "h", run: onHide },
  ];
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  // Raw anchor first, replaced by the fitted position before the first paint (layout effect):
  // near the window edge the menu slides fully inside instead of clipping.
  const [pos, setPos] = useState({ left: menu.x, top: menu.y });
  useEffect(() => { refs.current[idx]?.focus(); }, [idx]);
  useLayoutEffect(() => {
    const el = listRef.current;
    if (!el) return;
    el.showPopover(); // light dismiss + Esc + top layer come from the popover
    const r = el.getBoundingClientRect();
    const fit = fitMenuRect(menu.x, menu.y, r.width, r.height, window.innerWidth, window.innerHeight);
    setPos((p) => (fit.left !== p.left || fit.top !== p.top ? fit : p));
  }, [listRef, menu.x, menu.y]);

  const move = (to: number) => setIdx((to + items.length) % items.length);
  const onKey = (e: React.KeyboardEvent) => {
    switch (e.key) {
      case "ArrowDown": e.preventDefault(); e.stopPropagation(); move(idx + 1); break;
      case "ArrowUp": e.preventDefault(); e.stopPropagation(); move(idx - 1); break;
      case "Home": e.preventDefault(); e.stopPropagation(); setIdx(0); break;
      case "End": e.preventDefault(); e.stopPropagation(); setIdx(items.length - 1); break;
      case "Enter": case " ":
        e.preventDefault(); e.stopPropagation();
        items[idx]?.run();
        break;
      default: {
        // Access letter (O / A / L / C / H): straight to the action.
        const at = items.findIndex((it) => it.acc === e.key.toLowerCase());
        if (at >= 0) { e.preventDefault(); e.stopPropagation(); items[at]!.run(); }
      }
    }
  };

  const underline = (label: string, acc: string) => {
    const at = label.toLowerCase().indexOf(acc);
    return at < 0 ? label : <>{label.slice(0, at)}<u>{label[at]}</u>{label.slice(at + 1)}</>;
  };

  return (
    <>
      {/* Shield only: the popover's own light dismiss closes the menu; this keeps the dismissing
          click from reaching the row underneath (a native context menu passes no click either). */}
      <div className="ctx-scrim" onMouseDown={(e) => { e.preventDefault(); e.stopPropagation(); }}
           onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); onClose(); }} />
      <div className="ctx-menu" popover="auto" style={pos} role="menu" aria-label="Launcher actions" ref={listRef}
           onKeyDown={onKey} onToggle={(e) => { if (e.newState === "closed") onClose(); }}>
        {items.map((it, i) => (
          <button key={it.id + i} role="menuitem" tabIndex={i === idx ? 0 : -1}
                  ref={(el) => { refs.current[i] = el; }}
                  onClick={() => it.run()} onMouseEnter={() => setIdx(i)}>
            {underline(it.label, it.acc)}
          </button>
        ))}
      </div>
    </>
  );
}
