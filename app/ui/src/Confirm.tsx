import { useCallback, useEffect, useRef, useState } from "react";
import type { ConfirmAsk } from "./confirmLogic";

/** The shared "are you sure" gate for destructive actions (recycle, folder removal): one native
 *  modal <dialog>, same shape as the F1 cheat sheet — the browser contains Tab and paints the
 *  ::backdrop veil. The verb button answers yes (it holds the focus, so Enter confirms), Esc,
 *  Cancel and a backdrop click answer no.
 *
 *  The app's global Esc chains own the Esc key and preventDefault the native cancel away, so an
 *  owner must yield while `open` (Library does; panels check for a `dialog` ancestor and return).
 *  For small Settings rows where a modal outweighs the action, `ConfirmButton` is the two-step
 *  button form of the same idea. Every ask here is destructive, so the verb is the one red-filled
 *  button in the app; Cancel is the neutral secondary. */
export function useConfirm() {
  const [ask, setAsk] = useState<ConfirmAsk | null>(null);
  const settle = useRef<((ok: boolean) => void) | null>(null);
  const askIt = useCallback((a: ConfirmAsk) => new Promise<boolean>((done) => {
    settle.current?.(false); // a new ask while one is open: the unanswered one counts as cancelled
    settle.current = done;
    setAsk(a);
  }), []);
  const answer = useCallback((ok: boolean) => {
    settle.current?.(ok);
    settle.current = null;
    setAsk(null);
  }, []);
  useEffect(() => () => settle.current?.(false), []); // unmount mid-ask: the caller sees a cancel
  const node = <ConfirmDialog ask={ask} onConfirm={() => answer(true)} onClose={() => answer(false)} />;
  return { ask, open: ask !== null, askIt, dismiss: () => answer(false), node };
}

function ConfirmDialog({ ask, onConfirm, onClose }: { ask: ConfirmAsk | null; onConfirm: () => void; onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  // The exit animation keeps the closed dialog painted for 160ms; rendering the last ask stops it
  // from emptying itself the moment the answer lands (the dialog is display:none whenever ask is null,
  // so stale content is never visible except inside that fade).
  const last = useRef<ConfirmAsk | null>(null);
  if (ask) last.current = ask;
  const shown = ask ?? last.current;
  useEffect(() => {
    if (ask) ref.current?.showModal();
    else ref.current?.close(); // also fires the DOM onClose → answer(false) again, which is a no-op then
  }, [ask]);
  return (
    <dialog ref={ref} className="confirm" aria-label={shown?.title ?? "Confirm"}
            onCancel={onClose} onClose={onClose}
            onMouseDown={(e) => { if (e.target === ref.current) onClose(); /* a click on the ::backdrop */ }}>
      {shown && (
        <>
          <div className="confirm-body">
            <h3>{shown.title}</h3>
            {shown.body && <div className="cpath" title={shown.body}>{shown.body}</div>}
            {shown.items && (
              <ul className="confirm-list">
                {shown.items.map((i) => <li key={i}>{i}</li>)}
                {shown.more ? <li className="cmore">… and {shown.more.toLocaleString("en-US")} more</li> : null}
              </ul>
            )}
            {shown.note && <div className="confirm-note">{shown.note}</div>}
          </div>
          <div className="confirm-btns">
            {/* focus lands on Cancel (showModal starts at the first button): Enter answers no —
                going through with it is a deliberate Tab+Enter or a click */}
            <button className="btn secondary" onClick={onClose}>Cancel</button>
            <button className="btn danger" onClick={onConfirm}>{shown.verb}</button>
          </div>
        </>
      )}
    </dialog>
  );
}

/** Two-step destructive button (the Collections delete pattern, generalized): the first click arms
 *  it ("Sure? …"), the second goes through; three seconds of idleness disarms. Used where a modal
 *  would outweigh the action (Settings' Clear buttons). */
export function ConfirmButton({ label, confirmLabel, onConfirm, disabled, title }: {
  label: string;
  /** The armed label ("Sure? Clear the log"). */
  confirmLabel: string;
  onConfirm: () => void;
  disabled?: boolean;
  title?: string;
}) {
  const [armed, setArmed] = useState(false);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(t.current), []);
  const click = () => {
    if (disabled) return;
    if (!armed) {
      window.clearTimeout(t.current);
      setArmed(true);
      t.current = window.setTimeout(() => setArmed(false), 3000);
      return;
    }
    window.clearTimeout(t.current);
    setArmed(false);
    onConfirm();
  };
  return (
    <button className={"gbtn" + (armed ? " gbtn-arm" : "")} onClick={click} disabled={disabled}
            title={armed ? "Click again to go through with it" : title}>
      {armed ? confirmLabel : label}
    </button>
  );
}
