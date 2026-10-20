/** Tab wraps inside a modal panel (PRD B8): Shift+Tab on the first control moves to the last,
 *  Tab on the last wraps to the first. Attach to the panel root's onKeyDown. */
export function trapKey(e: React.KeyboardEvent): void {
  if (e.key !== "Tab") return;
  const root = e.currentTarget as HTMLElement;
  const items = Array.from(
    root.querySelectorAll<HTMLElement>("button, input, select, textarea, a[href], [tabindex]:not([tabindex='-1'])"),
  ).filter((el) => !el.hasAttribute("disabled") && el.offsetParent !== null);
  if (items.length === 0) return;
  const first = items[0];
  const last = items[items.length - 1];
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
}
