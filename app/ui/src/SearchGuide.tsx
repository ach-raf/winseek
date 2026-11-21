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
  // A press inside the guide keeps the caret in the search box and must not start a text selection —
  // but the body's scrollbar is styled with ::-webkit-scrollbar, and Blink treats a scrollbar press
  // as the default action of mousedown: a blanket preventDefault here would cancel dragging the
  // thumb. Presses in the scrollbar's own lane (its real rendered width) are left to the engine.
  const onDown = (e: React.MouseEvent) => {
    const el = (e.target as HTMLElement).closest?.(".sg-body") as HTMLElement | null;
    const lane = el ? el.offsetWidth - el.clientWidth : 0; // the scrollbar's width; 0 = not scrollable
    if (el && lane > 0 && e.clientX >= el.getBoundingClientRect().right - lane) return;
    e.preventDefault();
  };
  return (
    <div className="sg" ref={ref} role="dialog" aria-label="Narrow your search" onMouseDown={onDown}>
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
