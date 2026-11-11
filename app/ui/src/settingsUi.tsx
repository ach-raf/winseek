import type { ReactNode } from "react";

/** Shared v4 settings building blocks (prototype .grp / .gcard / .grow / .sw / .seg). Styles: settings.css. */

/** A titled group: h3 + rounded card. `note` is a small hint under the card. */
export function Grp({ title, note, label, children }: { title: string; note?: ReactNode; label?: string; children: ReactNode }) {
  return (
    <section className="grp" aria-label={label ?? title}>
      <h3>{title}</h3>
      <div className="gcard">{children}</div>
      {note && <p className="gnote">{note}</p>}
    </section>
  );
}

/** One card row: label (+ short description) on the left, the control on the right. */
export function Row({ title, desc, wrap, hint, children }: {
  title: ReactNode; desc?: ReactNode; wrap?: boolean; hint?: string; children?: ReactNode;
}) {
  return (
    <div className={"grow" + (wrap ? " wrap" : "")} title={hint}>
      <div className="gtxt"><b>{title}</b>{desc ? <span>{desc}</span> : null}</div>
      {children != null && <div className="gctl">{children}</div>}
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
