import type { ModelGroup, ModelsPlan } from "./types";

/** What a Settings model row offers when its component is not installed. */
export type RowAction =
  | { kind: "installed" }
  /** Missing, nothing running: a button (`bytes` null when the size is only learned once it starts). */
  | { kind: "download"; bytes: number | null }
  /** This row's files are coming down now. `frac` null until a size is known. */
  | { kind: "downloading"; got: number; total: number; frac: number | null }
  /** Missing while another row downloads (one download at a time). */
  | { kind: "waiting" }
  /** The last download of this row stopped: `error` is "cancelled" or the failure text. */
  | { kind: "failed"; error: string; bytes: number | null };

export interface DownloadState {
  plan: ModelsPlan | null;
  running: boolean;
  /** Groups of the running (or last) download; null when it was started elsewhere (first-run auto-download). */
  groups: ModelGroup[] | null;
  /** Bytes per plan item name, from the progress events of this download. */
  got: Record<string, number>;
  interrupted: string | null;
}

/** The first-run download (and the Setup panel without the parser box). */
export const DEFAULT_GROUPS: ModelGroup[] = ["llama", "embedder", "pdfium"];

/** Missing plan items of a group. */
const missingOf = (plan: ModelsPlan, g: ModelGroup) => plan.items.filter((i) => i.group === g && !i.present);

/** The row state of `g`. `installed` comes from the status page (what is on disk right now); the plan gives sizes. */
export function rowAction(g: ModelGroup, installed: boolean, s: DownloadState): RowAction {
  const missing = s.plan ? missingOf(s.plan, g) : [];
  if (installed || (s.plan && missing.length === 0)) return { kind: "installed" };
  const known = missing.every((i) => i.bytes != null);
  const bytes = missing.length && known ? missing.reduce((a, i) => a + (i.bytes ?? 0), 0) : null;
  const mine = (s.groups ?? DEFAULT_GROUPS).includes(g);
  if (s.running) {
    if (!mine) return { kind: "waiting" };
    const got = missing.reduce((a, i) => a + Math.min(s.got[i.name] ?? 0, i.bytes ?? Infinity), 0);
    // An item whose size the manifest lacks (llama.cpp zips) counts what has arrived so far.
    const total = missing.reduce((a, i) => a + (i.bytes ?? s.got[i.name] ?? 0), 0);
    return { kind: "downloading", got, total, frac: known && total > 0 ? got / total : null };
  }
  if (s.interrupted && mine) return { kind: "failed", error: s.interrupted, bytes };
  return { kind: "download", bytes };
}
