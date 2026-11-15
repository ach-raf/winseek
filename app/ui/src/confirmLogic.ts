/** Text for the shared "are you sure" gate (Confirm.tsx renders it). Pure on purpose: the
 *  node --test suite can check the wording shapes without a DOM. */

/** What a confirm dialog shows. */
export interface ConfirmAsk {
  /** The question ("Move 3 files to the Recycle Bin?"). */
  title: string;
  /** One line under the title — usually the full path or what the action forgets. */
  body?: string;
  /** The first few affected names; `more` counts the rest ("… and 12 more"). */
  items?: string[];
  more?: number;
  /** The confirm button's verb ("Recycle", "Remove", "Clear"). */
  verb: string;
  /** Small print at the bottom (the way back, e.g. the Recycle Bin undo). */
  note?: string;
}

/** Names shown before "… and N more". */
export const CONFIRM_MAX_ITEMS = 5;

const nameOf = (p: string) => p.slice(p.lastIndexOf("\\") + 1);

/** Ask text for recycling files. Every recycle in the Library window funnels through one gate
 *  (Library's LibActions.recycle), so this wording covers single hits and whole batch moves. */
export function recycleAsk(paths: string[]): ConfirmAsk {
  const n = paths.length;
  return {
    title: n === 1
      ? `Move “${nameOf(paths[0]!)}” to the Recycle Bin?`
      : `Move ${n.toLocaleString("en-US")} files to the Recycle Bin?`,
    body: n === 1 ? paths[0] : undefined,
    items: n > 1 ? paths.slice(0, CONFIRM_MAX_ITEMS).map(nameOf) : undefined,
    more: n > CONFIRM_MAX_ITEMS ? n - CONFIRM_MAX_ITEMS : undefined,
    verb: "Recycle",
    note: "Nothing is lost right away: the files sit in the Recycle Bin, and Ctrl+Z brings the last move back.",
  };
}

/** Ask text for removing an indexed folder: unlike unchecking, this forgets what was indexed. */
export function removeFolderAsk(path: string): ConfirmAsk {
  return {
    title: `Remove ${path} from the index?`,
    body: "Stops indexing the folder and forgets the files already indexed. Unchecking the folder instead keeps its data for a quick re-check.",
    verb: "Remove",
    note: "Your files on disk are not touched.",
  };
}
