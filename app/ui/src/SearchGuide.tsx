import { useEffect, useRef } from "react";
import type { Parsed } from "./filters";
import { applyTerm, GUIDE, isActive, type GuideTerm } from "./guideLogic";

/** "Narrow your search": every filter the box understands, one click each. Plain-word rows become
 *  chips in the bar (click again to drop); Power syntax rows are typed into the box for the user
 *  to finish. Esc / outside click closes; the caret always returns to the search field. */
export function SearchGuide({ query, parsed, onQuery, onClose }: {
  query: string; parsed: Parsed; onQuery: (q: string) => void; onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const down = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (ref.current && t && !ref.current.contains(t) && !t.closest(".sg-btn")) onClose();
    };
    document.addEventListener("mousedown", down);
    return () => document.removeEventListener("mousedown", down);
  }, [onClose]);
  const pick = (t: GuideTerm) => onQuery(applyTerm(query, parsed, t));
  return (
    <div className="sg" ref={ref} role="dialog" aria-label="Narrow your search" onMouseDown={(e) => e.preventDefault()}>
      <header className="sg-head">
        <b>Narrow your search</b>
        <span>Or just type it: <i>pdf invoices from last summer</i></span>
      </header>
      <div className="sg-body">
        {GUIDE.map((g) => (
          <section key={g.id} className={"sg-sec " + g.id}>
            <h4>{g.title}<small>{g.blurb}</small></h4>
            <div className="sg-terms">
              {g.terms.map((t) => (
                <button key={t.label} className={"sg-t" + (isActive(parsed, t) ? " on" : "") + (g.id === "advanced" ? " code" : "")}
                        tabIndex={-1} title={t.hint} aria-pressed={t.type ? isActive(parsed, t) : undefined} onClick={() => pick(t)}>
                  {t.label}
                  {g.id === "advanced" && t.hint && <em>{t.hint}</em>}
                </button>
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}
