import { ISearch, IX } from "./icons";

/** One small "filter this list by name" field, shared by every long list (timeline, collection,
 *  clean-up lists, disk-space files). The styling lives in base.css next to the other shared recipes. */
export function NameFilter({ value, onChange, label, placeholder = "Filter by name" }: {
  value: string;
  onChange: (v: string) => void;
  /** Accessible name (what this field filters). */
  label: string;
  placeholder?: string;
}) {
  return (
    <span className="nfilter">
      <ISearch size={14} />
      <input value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} aria-label={label}
             spellCheck={false} autoComplete="off" />
      {value !== "" && (
        <button className="nfx" tabIndex={-1} title="Clear the filter" aria-label="Clear the filter" onClick={() => onChange("")}>
          <IX size={10} />
        </button>
      )}
    </span>
  );
}
