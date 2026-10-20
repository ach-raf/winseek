import { useEffect, useRef, type RefObject } from "react";
import type { LaunchHit } from "./types";

/** The launcher context menu (PRD B5): full keyboard support — the first item is focused on
 *  open, ↑ ↓ (wrap) Home End move, Enter/Space activate, and the underlined access letters
 *  (Open O, admin A, location L, Copy C, Hide H) jump straight to the action. */
export function LaunchMenu({ menu, idx, setIdx, listRef, onRun, onHide, onClose }: {
  menu: { hit: LaunchHit; x: number; y: number };
  idx: number;
  setIdx: (i: number) => void;
  listRef: RefObject<HTMLDivElement | null>;
  onRun: (mode: "open" | "admin" | "reveal" | "copy") => void;
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
    { id: "hide", label: "Hide from results", acc: "h", run: onHide },
  ];
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  useEffect(() => { refs.current[idx]?.focus(); }, [idx]);

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
        if (at >= 0) { e.preventDefault(); e.stopPropagation(); items[at].run(); }
      }
    }
  };

  const underline = (label: string, acc: string) => {
    const at = label.toLowerCase().indexOf(acc);
    return at < 0 ? label : <>{label.slice(0, at)}<u>{label[at]}</u>{label.slice(at + 1)}</>;
  };

  return (
    <div className="ctx-overlay" onMouseDown={onClose}
         onContextMenu={(e) => { e.preventDefault(); onClose(); }}>
      <div className="ctx-menu" style={{ left: menu.x, top: menu.y }} role="menu" aria-label="Launcher actions" ref={listRef}
           onKeyDown={onKey} onMouseDown={(e) => e.stopPropagation()}>
        {items.map((it, i) => (
          <button key={it.id + i} role="menuitem" tabIndex={i === idx ? 0 : -1}
                  ref={(el) => { refs.current[i] = el; }}
                  onClick={() => it.run()} onMouseEnter={() => setIdx(i)}>
            {underline(it.label, it.acc)}
          </button>
        ))}
      </div>
    </div>
  );
}
