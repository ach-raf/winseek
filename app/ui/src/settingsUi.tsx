import { cloneElement, isValidElement, useId, type ReactElement, type ReactNode } from "react";

/** Shared v4 settings building blocks (prototype .grp / .gcard / .grow / .sw / .seg). Styles: settings.css. */

/** A titled group: h3 + rounded card. `note` is a small hint under the card. `id` marks it as a
 *  "Find a setting" jump target (data-setting). */
export function Grp({ title, note, label, id, children }: { title: string; note?: ReactNode; label?: string; id?: string; children: ReactNode }) {
  return (
    <section className="grp" aria-label={label ?? title} data-setting={id}>
      <h3>{title}</h3>
      <div className="gcard">{children}</div>
      {note && <p className="gnote">{note}</p>}
    </section>
  );
}

/** One card row: label (+ short description) on the left, the control on the right. When there
 *  is a description it is wired to the control with aria-describedby, so a screen reader reads
 *  the row's explanation when the control gets focus. `id` marks it as a "Find a setting" jump
 *  target (data-setting). */
export function Row({ title, desc, wrap, hint, id, children }: {
  title: ReactNode; desc?: ReactNode; wrap?: boolean; hint?: string; id?: string; children?: ReactNode;
}) {
  const descId = useId();
  const ctl = children != null && isValidElement(children) && desc != null
    ? cloneElement(children as ReactElement<{ "aria-describedby"?: string }>, { "aria-describedby": descId })
    : children;
  return (
    <div className={"grow" + (wrap ? " wrap" : "")} title={hint} data-setting={id}>
      <div className="gtxt"><b>{title}</b>{desc ? <span id={descId}>{desc}</span> : null}</div>
      {children != null && <div className="gctl">{ctl}</div>}
    </div>
  );
}

export const Switch = ({ checked, disabled, onChange, label }: {
  checked: boolean; disabled?: boolean; onChange: (v: boolean) => void; label: string;
}) => (
  <label className="switch">
    <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} aria-label={label} />
  </label>
);

export function Seg<T extends string>({ value, options, onChange, disabled, label }: {
  value: T | null; options: { id: T; label: string }[]; onChange: (v: T) => void; disabled?: boolean; label: string;
}) {
  return (
    <div className="seg" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <button key={o.id} role="radio" aria-checked={o.id === value} disabled={disabled}
                className={o.id === value ? "on" : ""} onClick={() => onChange(o.id)}>{o.label}</button>
      ))}
    </div>
  );
}

export const XIcon = ({ size = 11 }: { size?: number }) => (
  <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden><path d="M18 6 6 18M6 6l12 12" /></svg>
);
