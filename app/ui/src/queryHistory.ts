/** Search history for Alt+Up / Alt+Down (PRD B4): a deduped, newest-first list capped at 50,
 *  persisted in localStorage. Pure; the App wires it to the input. */

const KEY = "winseek.queryHistory";
const CAP = 50;

/** Record a query the user acted on. Empty or 1-char queries are noise; the same query twice
 *  in a row moves to the front instead of duplicating. */
export function push(list: string[], q: string): string[] {
  const t = q.trim();
  if (t.length < 2) return list;
  const out = list.filter((x) => x !== t);
  out.unshift(t);
  return out.slice(0, CAP);
}

/** One step through the history: dir −1 = older, +1 = newer. `at: -1` is the live input;
 *  stepping newer past the newest returns the draft the user was typing, stepping older past
 *  the oldest stays on the oldest. */
export function step(list: string[], at: number, draft: string, dir: -1 | 1): { q: string; at: number } {
  if (list.length === 0 || (dir === 1 && at < 0)) return { q: draft, at: -1 };
  if (dir === -1) {
    const next = at < 0 ? 0 : Math.min(at + 1, list.length - 1);
    return { q: list[next], at: next };
  }
  const next = at - 1;
  return next < 0 ? { q: draft, at: -1 } : { q: list[next], at: next };
}

export function load(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string").slice(0, CAP) : [];
  } catch {
    return []; // corrupted or blocked storage: an empty history, never a broken popup
  }
}

export function save(list: string[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(list.slice(0, CAP)));
  } catch {
    // Storage may be full or blocked; history is a comfort feature, losing it is fine.
  }
}
