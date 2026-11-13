import type { FolderEstimate } from "./types.ts";
import { formatDuration, formatRate, formatSize } from "./format.ts";

const fmt = (n: number) => n.toLocaleString("en-US");

/** One honest sentence for the add-folder confirmation: what was counted, what was measured, and what was not. */
export function estimateText(e: FolderEstimate): string {
  if (e.files === 0 && !e.capped) return "Nothing to index in this folder (no pictures, documents or PDFs).";
  const what = `${e.capped ? "At least " : ""}${fmt(e.files)} file${e.files === 1 ? "" : "s"} (${formatSize(e.bytes)})`;
  const partial = e.capped ? " The count stopped after a few seconds, so the real total is higher." : "";
  if (e.secs == null) return `${what}. No time estimate yet: this PC has not indexed anything, so its speed is unknown.${partial}`;
  if (e.basis === "observed") {
    return `${what}: about ${formatDuration(e.secs)}${e.rate ? ` at the ${formatRate(e.rate)} this PC managed before` : ""}.${partial}`;
  }
  return `${what}: at least ${formatDuration(e.secs)}. That is the measured reading time here; the meaning step adds more, and its speed is unknown until it has run once.${partial}`;
}
