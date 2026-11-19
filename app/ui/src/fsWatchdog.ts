import type { VolumeStatus } from "./types.ts";

/**
 * The safety net under the push channel. Every file the index gains or loses also moves a volume's file / folder
 * count, and the status is polled every 1-2 s. When the counts move and NO `fs-delta` showed up around that time,
 * the event stream missed something (the feed was down, a window slept, the app started mid-change): the lists
 * are re-read once the old way. A healthy stream never trips it. Pure state machine, node-testable.
 */

/** How long after a count change a late delta may still arrive and clear the suspicion. */
export const GRACE_MS = 3000;
/** A delta this recent before the count change explains it (events come first, the poll sees the count after). */
export const EXPLAINED_MS = 5000;
/** Never re-read more often than this. */
export const MIN_GAP_MS = 10000;

/** Counts of every volume; changes whenever an index count moved or a volume came / went. */
export function statusSignature(vols: readonly VolumeStatus[]): string {
  return vols.map((v) => `${v.letter}:${v.files}:${v.folders}`).join("|");
}

/** A scan / rescan / error is in progress: counts move wildly and the backend sends its own resync when it ends. */
export const isBusy = (vols: readonly VolumeStatus[]): boolean => vols.some((v) => v.state !== "live");

export class CountWatchdog {
  private last: string | null = null;
  private lastDeltaAt = -Infinity;
  private pendingSince: number | null = null;
  private lastFired = -Infinity;
  private wasBusy = false;

  /** Any delta (the backend's or the app's own) was applied. */
  onDelta(now: number): void {
    this.lastDeltaAt = now;
    this.pendingSince = null;
  }

  /** A status poll landed. Returns true when the lists should be re-read now. */
  onStatus(vols: readonly VolumeStatus[], now: number): boolean {
    if (vols.length === 0) return false;
    const sig = statusSignature(vols);
    if (isBusy(vols)) {
      this.last = sig; // the scan's own resync follows; do not judge counts mid-scan
      this.pendingSince = null;
      this.wasBusy = true;
      return false;
    }
    if (this.last === null || this.wasBusy) { // first look, or the first one after a scan: the baseline, not a change
      this.last = sig;
      this.wasBusy = false;
      return false;
    }
    if (sig !== this.last) {
      this.last = sig;
      if (now - this.lastDeltaAt > EXPLAINED_MS) this.pendingSince ??= now;
    }
    if (this.pendingSince !== null && now - this.pendingSince >= GRACE_MS && now - this.lastFired >= MIN_GAP_MS) {
      this.pendingSince = null;
      this.lastFired = now;
      return true;
    }
    return false;
  }
}
