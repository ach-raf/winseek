import { byScope, hint, KEYMAP } from "./keymap";

/** The F1 / Ctrl+/ cheat sheet (PRD RB2): every shortcut of the current window, grouped, read
 *  from the one keymap table. Global hotkeys show the current binding once Settings can rebind
 *  them (`bindings`); until then the keymap defaults. */
export function Shortcuts({ scope, bindings, onClose }: { scope: "popup" | "library"; bindings?: Record<string, string>; onClose: () => void }) {
  const groups = scope === "popup" ? [...byScope("global"), ...byScope("popup")] : byScope(scope);
  return (
    <div className="cheat-overlay" onMouseDown={onClose}>
      <div className="cheat" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts" onMouseDown={(e) => e.stopPropagation()}>
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
                      {(bindings?.[k.id] ? [bindings[k.id]] : k.keys).map((c, i) => (
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
      </div>
    </div>
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
