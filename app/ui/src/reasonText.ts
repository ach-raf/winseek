/** Plain-words wording for the "Failed" and "Skipped" lists in Settings. Input is a stable reason
 *  code (never a library message); anything unknown reads as a generic sentence, so no technical
 *  text ever reaches the screen. The raw detail stays in a tooltip. */

interface Wording {
  /** Singular sentence ("{n}" is the count). */
  one: string;
  /** Plural sentence. */
  many: string;
  /** What the person can do; absent when there is nothing to do. */
  hint?: string;
}

const W: Record<string, Wording> = {
  // failed
  damaged_image: { one: "{n} picture is damaged or not really a picture", many: "{n} pictures are damaged or not really pictures" },
  damaged_video: { one: "{n} video is incomplete or damaged", many: "{n} videos are incomplete or damaged" },
  damaged_audio: { one: "{n} audio file is incomplete or damaged", many: "{n} audio files are incomplete or damaged" },
  damaged_pdf: { one: "{n} PDF is incomplete or damaged", many: "{n} PDFs are incomplete or damaged" },
  damaged_archive: { one: "{n} Office or zip file is damaged", many: "{n} Office or zip files are damaged" },
  damaged_document: { one: "{n} Office or zip file is damaged", many: "{n} Office or zip files are damaged" },
  unreadable: { one: "{n} file could not be opened", many: "{n} files could not be opened", hint: "Another program may be using them, or this account may not be allowed to read them." },
  timed_out: { one: "{n} audio or video file took too long to read", many: "{n} audio or video files took too long to read" },
  embed_failed: { one: "{n} file could not be added to search by meaning", many: "{n} files could not be added to search by meaning" },
  failed: { one: "{n} file could not be read", many: "{n} files could not be read" },
  // skipped
  not_an_image: { one: "{n} file is not really a picture", many: "{n} files are not really pictures" },
  empty_file: { one: "{n} file is empty", many: "{n} files are empty" },
  temp_file: { one: "{n} temporary file is ignored", many: "{n} temporary files are ignored" },
  still_writing: { one: "{n} file is still being written; it is checked again later", many: "{n} files are still being written; they are checked again later" },
  cloud_only: { one: "{n} file is online-only and not on this PC", many: "{n} files are online-only and not on this PC", hint: "Make them available offline to include them." },
  no_heic_codec: { one: "{n} HEIC photo needs a Windows codec", many: "{n} HEIC photos need a Windows codec", hint: "Install HEIF Image Extensions from the Microsoft Store." },
  no_ffmpeg: { one: "{n} audio or video file is waiting for the audio and video tool", many: "{n} audio and video files are waiting for the audio and video tool" },
  too_large: { one: "{n} file is too large to read", many: "{n} files are too large to read" },
  binary: { one: "{n} file holds no text", many: "{n} files hold no text" },
  no_text: { one: "{n} file has no readable text", many: "{n} files have no readable text" },
  legacy_office: { one: "{n} old Office file (.doc, .xls, .ppt) is not read", many: "{n} old Office files (.doc, .xls, .ppt) are not read", hint: "Save them as .docx, .xlsx or .pptx to include them." },
  password_protected: { one: "{n} file is password-protected", many: "{n} files are password-protected", hint: "Remove the password to include them." },
  no_audio_model: { one: "{n} audio file cannot be read by the installed model", many: "{n} audio files cannot be read by the installed model" },
  no_media_content: { one: "{n} audio or video file has no usable sound or picture", many: "{n} audio or video files have no usable sound or picture" },
  denied: { one: "{n} private or system file is never read", many: "{n} private or system files are never read" },
  zip_bomb: { one: "{n} archive is too big to open safely", many: "{n} archives are too big to open safely" },
  changed: { one: "{n} file changed while it was being read", many: "{n} files changed while they were being read" },
  not_notebook: { one: "{n} notebook is not valid", many: "{n} notebooks are not valid" },
};

const GENERIC: Wording = W.failed;

const fmt = (n: number) => n.toLocaleString("en-US");

/** True for a code this app writes (anything else is old free text or unknown). */
export const isReasonCode = (r: string) => Object.prototype.hasOwnProperty.call(W, r);

/** One plain sentence with the count, e.g. "31 pictures are damaged or not really pictures". */
export function reasonSentence(reason: string, count: number): string {
  const w = isReasonCode(reason) ? W[reason] : GENERIC;
  return (count === 1 ? w.one : w.many).replace("{n}", fmt(count));
}

/** What the person can do about this group, or null when there is nothing to do. */
export function reasonHint(reason: string): string | null {
  return isReasonCode(reason) ? W[reason].hint ?? null : null;
}

/** The file name of a path, for the example list. */
export const fileName = (p: string) => p.split(/[\\/]/).pop() || p;
