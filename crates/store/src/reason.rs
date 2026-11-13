//! Why a file was not indexed, as a short stable code plus the raw technical detail.
//!
//! The `files.error` column holds the code (`damaged_image`, `too_large`, ...), `files.detail` the
//! raw message for a tooltip. Grouping by code keeps library error text, pointers and absolute
//! paths out of what a person reads. Rows written by older versions carry free text; [`classify`]
//! maps those to codes (once by the schema migration, and at read time for rows a stale writer
//! left behind).

/// Every code the app writes or understands. Order is irrelevant.
pub const CODES: &[&str] = &[
    // skipped: the file is fine or not ours to read
    "cloud_only",
    "no_heic_codec",
    "no_ffmpeg",
    "too_large",
    "binary",
    "no_text",
    "legacy_office",
    "password_protected",
    "no_audio_model",
    "no_media_content",
    "denied",
    "zip_bomb",
    "changed",
    "not_notebook",
    "not_an_image",
    "empty_file",
    "temp_file",
    "still_writing",
    // failed: the file could not be read
    "damaged_image",
    "damaged_video",
    "damaged_audio",
    "damaged_pdf",
    "damaged_archive",
    "damaged_document",
    "unreadable",
    "embed_failed",
    "timed_out",
    "failed",
];

pub fn is_code(s: &str) -> bool {
    CODES.contains(&s)
}

/// Code for a stored reason or raw message. `kind` is the file's kind (`image`, `video`, `audio`,
/// `pdf`, `doc`, ...) when known: an ffmpeg message means "damaged video" or "damaged audio".
/// Codes map to themselves; the pre-code spellings map to their code.
pub fn classify(msg: &str, kind: &str) -> &'static str {
    let m = msg.trim();
    if let Some(c) = CODES.iter().find(|c| **c == m) {
        return c;
    }
    let l = m.to_ascii_lowercase();
    match l.as_str() {
        "encrypted" => return "password_protected",
        "file larger than 64 mb" => return "too_large",
        _ => {}
    }
    let media = |damaged_video: &'static str| if kind == "audio" { "damaged_audio" } else { damaged_video };
    if l.starts_with("decode") || l.starts_with("encode") {
        if l.contains("memory limit") || (l.contains("dimension") && l.contains("limit")) {
            "too_large"
        } else {
            "damaged_image"
        }
    } else if l.starts_with("ffmpeg") || l.starts_with("audio:") {
        if l.contains("timed out") {
            "timed_out"
        } else if l.contains("no such file") || l.contains("access is denied") || l.contains("permission denied") || l.contains("being used by another") {
            "unreadable"
        } else {
            media("damaged_video")
        }
    } else if l.starts_with("pdf") {
        "damaged_pdf"
    } else if l.starts_with("zip") || l.contains("eocd") || l.contains("invalid zip") {
        "damaged_archive"
    } else if l.starts_with("not a docx") || l.starts_with("not an xlsx") || l.starts_with("not a pptx") || l.starts_with("not an opendocument") {
        "damaged_document"
    } else if l.starts_with("read:") {
        "unreadable"
    } else if l.starts_with("embed") {
        "embed_failed"
    } else {
        "failed"
    }
}

/// `(code, detail)` to store for `msg`: a code keeps no detail, anything else keeps its text.
pub fn split(msg: &str, kind: &str) -> (&'static str, Option<String>) {
    let code = classify(msg, kind);
    if code == msg.trim() {
        (code, None)
    } else {
        (code, Some(msg.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quoted_reasons_map_to_codes() {
        let c = |m: &str, k: &str| classify(m, k);
        assert_eq!(c("decode: The image format could not be determined", "image"), "damaged_image");
        assert_eq!(c("decode: Format error decoding Gif: unknown block type encountered", "image"), "damaged_image");
        assert_eq!(c("decode: Format error decoding Gif: invalid code in LZW stream", "image"), "damaged_image");
        assert_eq!(c("zip: invalid Zip archive: Could not find EOCD", "doc"), "damaged_archive");
        assert_eq!(c("decode failed", "image"), "damaged_image");
        assert_eq!(c("decode: Memory limit exceeded", "image"), "too_large");
        let ff = "ffmpeg: [in#0 @ 00000222cd15a800] Error opening input: Invalid data found when processing input | Error opening input file C:\\Users\\x\\v.mp4. | Error opening input files: Invalid data found when processing input";
        assert_eq!(c(ff, "video"), "damaged_video");
        assert_eq!(c(ff, "audio"), "damaged_audio");
        assert_eq!(c("pdf: PdfiumLibraryInternalError(FormatError)", "pdf"), "damaged_pdf");
    }

    #[test]
    fn old_codes_and_other_messages() {
        assert_eq!(classify("encrypted", "pdf"), "password_protected");
        assert_eq!(classify("file larger than 64 MB", "image"), "too_large");
        assert_eq!(classify("cloud_only", "image"), "cloud_only");
        assert_eq!(classify("read: Access is denied. (os error 5)", "doc"), "unreadable");
        assert_eq!(classify("embed: server said no", "image"), "embed_failed");
        assert_eq!(classify("ffmpeg: timed out", "video"), "timed_out");
        assert_eq!(classify("not a docx (no word/document.xml)", "doc"), "damaged_document");
        assert_eq!(classify("something new", "doc"), "failed");
        assert_eq!(classify("", "doc"), "failed");
    }

    #[test]
    fn a_code_keeps_no_detail_and_text_keeps_it() {
        assert_eq!(split("too_large", "doc"), ("too_large", None));
        assert_eq!(split("zip: bad", "doc"), ("damaged_archive", Some("zip: bad".to_string())));
    }

    #[test]
    fn every_code_maps_to_itself() {
        for c in CODES {
            assert_eq!(classify(c, "doc"), *c);
        }
    }
}
