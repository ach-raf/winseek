import { useEffect, useRef } from "react";
import { byScope, hint, KEYMAP } from "./keymap";

/** The F1 / Ctrl+/ cheat sheet (PRD RB2): every shortcut of the current window, grouped, read
 *  from the one keymap table. A native modal <dialog>: the browser contains Tab inside the sheet
 *  and paints the veil via ::backdrop. The app's global Esc chain closes it first (it owns the
 *  key, preventDefault stops the native cancel), so onCancel/onClose here are the safety net.
 *
 *  The dialog stays mounted (driven by the `open` prop, like the confirm dialog) so closing can
 *  animate out of the top layer; unmounting it would cut the exit animation before it starts. */
export function Shortcuts({ scope, open, bindings, onClose }: { scope: "popup" | "library"; open: boolean; bindings?: Record<string, string>; onClose: () => void }) {
  const groups = scope === "popup" ? [...byScope("global"), ...byScope("popup")] : byScope(scope);
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open) { if (!d.open) d.showModal(); }
    else if (d.open) d.close();
  }, [open]);
  return (
    <dialog ref={ref} className="cheat" aria-label="Keyboard shortcuts"
            onCancel={onClose} onClose={onClose}
            onMouseDown={(e) => { if (e.target === ref.current) onClose(); /* a click on the ::backdrop */ }}>
      <header className="cheat-head">
        <b>Keyboard shortcuts</b>
        <span className="muted">Esc closes</span>
        <button className="btn quiet" onClick={onClose} aria-label="Close the shortcuts">Close</button>
      </header>
      <div className="cheat-cols">
        {groups.map((g) => (
          <section key={g.group}>
            <h3>{g.group}</h3>
            <ul>
              {g.items.map((k) => (
                <li key={k.id}>
                  <span className="cheat-keys">
                    {(bindings?.[k.id] ? [bindings[k.id]!] : k.keys).map((c, i) => (
                      <kbd key={`${c}-${i}`}>{c.replaceAll("+", " ")}</kbd>
                    ))}
                  </span>
                  <span className="cheat-label">{k.label}</span>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </dialog>
  );
}

/** Footer hints for the selected row kind (PRD RB3): keymap ids + the short verb to show. */
export type RowKind = "file" | "folder" | "app" | "setting" | "command" | "calc" | "preview";

const HINTS: Record<RowKind, [string, string][]> = {
  preview: [["popup.esc", "Close"], ["popup.move", "Next"], ["popup.open", "Open"]],
  file: [["popup.open", "Open"], ["popup.reveal", "Reveal"], ["popup.preview", "Preview"], ["popup.cheatsheet", "Keys"]],
  folder: [["popup.open", "Open"], ["popup.reveal", "Reveal"], ["popup.cheatsheet", "Keys"]],
  app: [["popup.open", "Launch"], ["popup.admin", "Admin"], ["popup.cheatsheet", "Keys"]],
  setting: [["popup.open", "Open"], ["popup.cheatsheet", "Keys"]],
  command: [["popup.open", "Run"], ["popup.cheatsheet", "Keys"]],
  calc: [["popup.open", "Copy"], ["popup.cheatsheet", "Keys"]],
};

export function footerHints(kind: RowKind): [string, string][] {
  return HINTS[kind];
}

/** `<kbd>…</kbd> Verb` for one footer hint. */
export function HintChip({ id, verb }: { id: string; verb: string }) {
  const h = hint(id);
  return h ? <span><kbd>{h.replaceAll("+", " ")}</kbd> {verb}</span> : null;
}

/** `title="<label> (<keys>)"` for buttons that have a shortcut (PRD B4: from the keymap). */
export function tooltip(id: string): string | undefined {
  const k = KEYMAP.find((x) => x.id === id);
  return k ? `${k.label} (${k.keys[0]})` : undefined;
}
