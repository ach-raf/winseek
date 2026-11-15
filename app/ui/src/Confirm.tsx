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
 *  button form of the same idea. */
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
  useEffect(() => {
    if (ask) ref.current?.showModal();
    else ref.current?.close(); // also fires the DOM onClose → answer(false) again, which is a no-op then
  }, [ask]);
  return (
    <dialog ref={ref} className="confirm" aria-label={ask?.title ?? "Confirm"}
            onCancel={onClose} onClose={onClose}
            onMouseDown={(e) => { if (e.target === ref.current) onClose(); /* a click on the ::backdrop */ }}>
      {ask && (
        <>
          <div className="confirm-body">
            <h3>{ask.title}</h3>
            {ask.body && <div className="cpath" title={ask.body}>{ask.body}</div>}
            {ask.items && (
              <ul className="confirm-list">
                {ask.items.map((i) => <li key={i}>{i}</li>)}
                {ask.more ? <li className="cmore">… and {ask.more.toLocaleString("en-US")} more</li> : null}
              </ul>
            )}
            {ask.note && <div className="confirm-note">{ask.note}</div>}
          </div>
          <div className="confirm-btns">
            {/* focus lands on Cancel (showModal starts at the first button): Enter answers no —
                going through with it is a deliberate Tab+Enter or a click */}
            <button className="btn" onClick={onClose}>Cancel</button>
            <button className="btn primary" onClick={onConfirm}>{ask.verb}</button>
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
