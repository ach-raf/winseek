//! Text extraction for the Tier 1 document / code scope (pure Rust, no native deps).
//!
//! - Office Open XML (`.docx/.xlsx/.pptx`, macro variants): zip + streaming XML. Paragraphs, cells
//!   and slide text come out as plain text; sheets and slides get a marker line ("Sheet: Name",
//!   "Slide 3") whose position is remembered in [`Extracted::marks`] for the chunk locators.
//! - OpenDocument (`.odt/.ods/.odp`): `content.xml`.
//! - Legacy binary Office (`.doc/.xls/.ppt`, or an OLE container under another name): skipped,
//!   reason `legacy_office`.
//! - RTF: control words stripped. Everything else: decoded as text (BOM / UTF-16 / UTF-8 / Latin-1),
//!   binary content is skipped.
//!
//! Limits: the produced text is capped at [`MAX_TEXT_BYTES`]; archives get a decompression budget
//! ([`MAX_INFLATED`]) so a zip bomb is skipped (`zip_bomb`) instead of exhausting memory or time.

use std::io::{BufRead, BufReader, Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

use crate::textdec::decode_text;

/// Files larger than this are skipped (`too_large`) without being read.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Extracted text is cut here (bytes).
pub const MAX_TEXT_BYTES: usize = 200 * 1024;
/// Plain-text files: only this prefix is decoded.
pub const TEXT_READ_CAP: usize = 2 * 1024 * 1024;
/// Total bytes that may be inflated from one archive.
pub const MAX_INFLATED: u64 = 128 * 1024 * 1024;
/// Largest single XML part read.
const MAX_PART: u64 = 96 * 1024 * 1024;
/// Shared-strings table cap (bytes of text kept).
const MAX_SHARED_BYTES: usize = 8 * 1024 * 1024;
/// Archives with more entries than this are not opened.
const MAX_ENTRIES: usize = 20_000;

/// Where a stretch of text came from inside the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loc {
    Slide(u32),
    Sheet(String),
}

/// `loc` applies from character offset `at` (in the extracted text) up to the next mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub at: usize,
    pub loc: Loc,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub text: String,
    pub marks: Vec<Mark>,
    /// The text hit [`MAX_TEXT_BYTES`] and was cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    /// Not an error: the file is recorded as skipped with this reason.
    Skip(&'static str),
    /// Unreadable / corrupt: the file is recorded as failed.
    Failed(String),
}

type Res<T> = Result<T, ExtractError>;

/// Accumulates text up to the cap and tracks marks by character offset.
struct Out {
    text: String,
    chars: usize,
    marks: Vec<Mark>,
    full: bool,
}

impl Out {
    fn new() -> Out {
        Out { text: String::new(), chars: 0, marks: Vec::new(), full: false }
    }

    fn full(&self) -> bool {
        self.full
    }

    fn push(&mut self, s: &str) {
        if self.full || s.is_empty() {
            return;
        }
        let room = MAX_TEXT_BYTES - self.text.len();
        if s.len() > room {
            let mut cut = room;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.push_str(&s[..cut]);
            self.chars += s[..cut].chars().count();
            self.full = true;
        } else {
            self.text.push_str(s);
            self.chars += s.chars().count();
            if self.text.len() >= MAX_TEXT_BYTES {
                self.full = true;
            }
        }
    }

    fn push_char(&mut self, c: char) {
        let mut b = [0u8; 4];
        self.push(c.encode_utf8(&mut b));
    }

    /// Start a new slide / sheet: marker line plus a mark at its first character.
    fn section(&mut self, loc: Loc, title: &str) {
        if self.full {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            self.push("\n");
        }
        self.marks.push(Mark { at: self.chars, loc });
        self.push(title);
        self.push("\n");
    }

    fn ends_with_newline(&self) -> bool {
        self.text.ends_with('\n')
    }

    fn finish(self) -> Extracted {
        Extracted { text: self.text, marks: self.marks, truncated: self.full }
    }
}

/// Extensions whose text [`extract_file`] reads as structured documents (the OOXML / ODF / RTF
/// branches, not the plain-text fallback). The Space preview offers the same files a document
/// text view, so the two stay in step through [`is_document_ext`].
pub const DOCUMENT_EXTS: &[&str] =
    &["docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "ppsx", "potx", "odt", "ott", "ods", "ots", "odp", "otp", "rtf"];

/// Whether `ext` (lowercase) is a structured-document format the extractor reads.
pub fn is_document_ext(ext: &str) -> bool {
    DOCUMENT_EXTS.contains(&ext)
}

fn ext_of(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Extract the searchable text of `bytes` (the whole file, already size-checked), choosing the
/// reader by `path`'s extension.
pub fn extract_file(path: &str, bytes: &[u8]) -> Res<Extracted> {
    if super::guard::is_temp_name(path) {
        return Err(ExtractError::Skip("temp_file"));
    }
    let ext = ext_of(path);
    let doc = match ext.as_str() {
        "doc" | "dot" | "xls" | "xlt" | "ppt" | "pps" | "pot" => return Err(ExtractError::Skip("legacy_office")),
        "docx" | "docm" | "dotx" | "dotm" => ooxml(bytes, Ooxml::Word)?,
        "xlsx" | "xlsm" | "xltx" | "xltm" => ooxml(bytes, Ooxml::Sheet)?,
        "pptx" | "pptm" | "ppsx" | "potx" => ooxml(bytes, Ooxml::Slides)?,
        "odt" | "ott" => odf(bytes, OdfKind::Text)?,
        "ods" | "ots" => odf(bytes, OdfKind::Sheet)?,
        "odp" | "otp" => odf(bytes, OdfKind::Slides)?,
        "rtf" => plain(bytes, true)?,
        // Notebooks: cell sources only, never the outputs (huge, and they leak data).
        "ipynb" => {
            let text = super::guard::notebook_text(bytes).ok_or(ExtractError::Skip("not_notebook"))?;
            plain(text.as_bytes(), false)?
        }
        _ => plain(bytes, false)?,
    };
    if doc.text.trim().is_empty() {
        return Err(ExtractError::Skip("no_text"));
    }
    Ok(doc)
}

// ---------------------------------------------------------------- plain text / RTF

fn plain(bytes: &[u8], rtf: bool) -> Res<Extracted> {
    let truncated = bytes.len() > TEXT_READ_CAP;
    let head = &bytes[..bytes.len().min(TEXT_READ_CAP)];
    let (text, _enc) = decode_text(head, truncated).ok_or(ExtractError::Skip("binary"))?;
    let text = if rtf { strip_rtf(&text) } else { text };
    let mut out = Out::new();
    // Normalise line endings; NULs (UTF-16 leftovers) and form feeds carry no text.
    let norm = text.replace("\r\n", "\n").replace('\r', "\n").replace(['\0', '\x0c'], " ");
    out.push(&norm);
    Ok(out.finish())
}

/// Windows-1252 0x80..=0x9F, the printable ones that matter in RTF `\'hh` escapes.
const CP1252_HIGH: [char; 32] = [
    '€', ' ', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', ' ', 'Ž', ' ', ' ', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›',
    'œ', ' ', 'ž', 'Ÿ',
];

/// Destination groups whose content is not document text.
const RTF_SKIP: &[&str] = &[
    "fonttbl", "colortbl", "stylesheet", "info", "pict", "object", "header", "footer", "headerl", "headerr", "footerl", "footerr", "themedata",
    "datastore", "latentstyles", "listtable", "listoverridetable", "rsidtbl", "generator", "xmlnstbl", "filetbl", "revtbl", "colorschememapping",
    "fldinst", "bkmkstart", "bkmkend", "private",
];

/// Strip RTF control words and destination groups, keep the text (`\par` -> newline, `\tab`, `\'hh`, `\uN`).
pub fn strip_rtf(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() / 2);
    // Stack of "this group is skipped" flags.
    let mut skip: Vec<bool> = vec![false];
    let mut uc_skip = 1usize; // \ucN: characters to ignore after a \uN escape
    let mut pending_skip = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let skipping = *skip.last().unwrap_or(&false);
        match c {
            '{' => {
                skip.push(skipping);
                i += 1;
            }
            '}' => {
                if skip.len() > 1 {
                    skip.pop();
                }
                i += 1;
            }
            '\\' => {
                i += 1;
                let Some(&n) = chars.get(i) else { break };
                if !n.is_ascii_alphabetic() {
                    i += 1;
                    match n {
                        '\\' | '{' | '}' => {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else if !skipping {
                                out.push(n);
                            }
                        }
                        '\'' => {
                            let hex: String = chars.iter().skip(i).take(2).collect();
                            i += hex.chars().count();
                            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                                if pending_skip > 0 {
                                    pending_skip -= 1;
                                } else if !skipping {
                                    out.push(match b {
                                        0x80..=0x9F => CP1252_HIGH[usize::from(b - 0x80)],
                                        _ => char::from(b),
                                    });
                                }
                            }
                        }
                        '*' => {
                            if let Some(f) = skip.last_mut() {
                                *f = true;
                            }
                        }
                        '~' => {
                            if !skipping {
                                out.push(' ');
                            }
                        }
                        '\n' | '\r' if !skipping => out.push('\n'),
                        _ => {}
                    }
                    continue;
                }
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // optional signed numeric parameter
                let num_start = i;
                if i < chars.len() && chars[i] == '-' {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let param: Option<i32> = chars[num_start..i].iter().collect::<String>().parse().ok();
                if i < chars.len() && chars[i] == ' ' {
                    i += 1; // the delimiter space belongs to the control word
                }
                if RTF_SKIP.contains(&word.as_str()) {
                    if let Some(f) = skip.last_mut() {
                        *f = true;
                    }
                    continue;
                }
                if skipping {
                    continue;
                }
                match word.as_str() {
                    "par" | "line" | "sect" | "page" | "row" => out.push('\n'),
                    "tab" | "cell" => out.push('\t'),
                    "emdash" => out.push('—'),
                    "endash" => out.push('–'),
                    "bullet" => out.push('•'),
                    "lquote" => out.push('‘'),
                    "rquote" => out.push('’'),
                    "ldblquote" => out.push('“'),
                    "rdblquote" => out.push('”'),
                    "uc" => uc_skip = param.unwrap_or(1).clamp(0, 8) as usize,
                    "u" => {
                        if let Some(n) = param {
                            let code = if n < 0 { (n + 65536) as u32 } else { n as u32 };
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            pending_skip = uc_skip;
                        }
                    }
                    _ => {}
                }
            }
            '\r' | '\n' => i += 1, // raw line breaks in RTF are insignificant
            _ => {
                if pending_skip > 0 {
                    pending_skip -= 1;
                } else if !skipping {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- zip helpers

struct Zip<'a> {
    ar: ZipArchive<Cursor<&'a [u8]>>,
    /// Remaining inflate budget.
    budget: u64,
    bomb: bool,
}

impl<'a> Zip<'a> {
    fn open(bytes: &'a [u8]) -> Res<Zip<'a>> {
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            // OLE compound file: legacy Office, or an encrypted OOXML package.
            return Err(ExtractError::Skip("legacy_office"));
        }
        let ar = ZipArchive::new(Cursor::new(bytes)).map_err(|e| ExtractError::Failed(format!("zip: {e}")))?;
        if ar.len() > MAX_ENTRIES {
            return Err(ExtractError::Skip("zip_bomb"));
        }
        Ok(Zip { ar, budget: MAX_INFLATED, bomb: false })
    }

    fn has(&mut self, name: &str) -> bool {
        self.ar.by_name(name).is_ok()
    }

    /// Stream the part `name` through `f`. `None` = no such part. A part that eats the whole
    /// remaining budget (or [`MAX_PART`]) marks the archive as a bomb.
    fn with_part<T>(&mut self, name: &str, f: impl FnOnce(&mut dyn BufRead) -> T) -> Option<T> {
        if self.bomb {
            return None;
        }
        let file = self.ar.by_name(name).ok()?;
        let limit = self.budget.min(MAX_PART);
        let mut rd = BufReader::new(file.take(limit));
        let r = f(&mut rd);
        let left = rd.get_ref().limit();
        self.budget -= limit - left;
        if left == 0 {
            // Consumed everything we were willing to inflate: only fine if that was the true end.
            let mut probe = [0u8; 1];
            if rd.get_mut().get_mut().read(&mut probe).unwrap_or(0) > 0 {
                self.bomb = true;
            }
        }
        Some(r)
    }

    fn check(&self) -> Res<()> {
        if self.bomb {
            Err(ExtractError::Skip("zip_bomb"))
        } else {
            Ok(())
        }
    }

    fn names_matching(&self, prefix: &str, suffix: &str) -> Vec<String> {
        self.ar.file_names().filter(|n| n.starts_with(prefix) && n.ends_with(suffix)).map(str::to_owned).collect()
    }
}

fn reader(r: &mut dyn BufRead) -> Reader<&mut dyn BufRead> {
    let mut rd = Reader::from_reader(r);
    rd.config_mut().check_end_names = false;
    rd
}

fn attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.local_name().as_ref() == local {
            return a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned());
        }
    }
    None
}

/// Value of the namespaced `prefix:local` attribute (e.g. `r:id`), ignoring a plain `local`.
fn prefixed_attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.local_name().as_ref() == local && a.key.as_ref().contains(':') {
            return a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned());
        }
    }
    None
}

fn local(e: &BytesStart<'_>) -> String {
    e.local_name().as_ref().to_owned()
}

fn local_end(e: &quick_xml::events::BytesEnd<'_>) -> String {
    e.local_name().as_ref().to_owned()
}

/// Resolve one of the five predefined entities / a character reference.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Join a relationship `target` with the folder of the part that owns the .rels file.
fn resolve_target(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(t) = target.strip_prefix('/') {
        return t.to_owned();
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// `(id -> target)` from a `.rels` part.
fn read_rels(z: &mut Zip<'_>, name: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    z.with_part(name, |r| {
        let mut rd = reader(r);
        let mut buf = Vec::new();
        loop {
            match rd.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(&e) == "Relationship" => {
                    if let (Some(id), Some(t)) = (attr(&e, "Id"), attr(&e, "Target")) {
                        map.insert(id, t);
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    });
    map
}

// ---------------------------------------------------------------- OOXML

#[derive(Clone, Copy)]
enum Ooxml {
    Word,
    Sheet,
    Slides,
}

fn ooxml(bytes: &[u8], kind: Ooxml) -> Res<Extracted> {
    let mut z = Zip::open(bytes)?;
    let mut out = Out::new();
    match kind {
        Ooxml::Word => {
            if !z.has("word/document.xml") {
                return Err(ExtractError::Failed("not a docx (no word/document.xml)".into()));
            }
            z.with_part("word/document.xml", |r| docx_text(r, &mut out));
        }
        Ooxml::Sheet => xlsx_text(&mut z, &mut out)?,
        Ooxml::Slides => pptx_text(&mut z, &mut out)?,
    }
    z.check()?;
    Ok(out.finish())
}

fn docx_text(r: &mut dyn BufRead, out: &mut Out) {
    let mut rd = reader(r);
    let mut buf = Vec::new();
    let mut in_t = false;
    while !out.full() {
        match rd.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local(&e).as_str() {
                "t" => in_t = true,
                "tab" => out.push_char('\t'),
                "br" | "cr" => out.push_char('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match local(&e).as_str() {
                "tab" => out.push_char('\t'),
                "br" | "cr" => out.push_char('\n'),
                _ => {}
            },
            Ok(Event::End(e)) => match local_end(&e).as_str() {
                "t" => in_t = false,
                "p" => out.push_char('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_t => out.push(&t.xml10_content()),
            Ok(Event::GeneralRef(g)) if in_t => {
                if let Some(c) = entity(&g.into_inner()) {
                    out.push_char(c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// `(name, relationship id)` of every `<tag ...>` in `part`.
fn list_refs(z: &mut Zip<'_>, part: &str, tag: &str) -> Vec<(Option<String>, String)> {
    let mut v = Vec::new();
    z.with_part(part, |r| {
        let mut rd = reader(r);
        let mut buf = Vec::new();
        loop {
            match rd.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(&e) == tag => {
                    if let Some(id) = prefixed_attr(&e, "id") {
                        v.push((attr(&e, "name"), id));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    });
    v
}

fn numbered(names: Vec<String>, prefix: &str) -> Vec<String> {
    let mut v: Vec<(u32, String)> = names
        .into_iter()
        .filter_map(|n| {
            let num = n.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

fn xlsx_text(z: &mut Zip<'_>, out: &mut Out) -> Res<()> {
    // Shared strings.
    let mut strings: Vec<String> = Vec::new();
    z.with_part("xl/sharedStrings.xml", |r| {
        let mut rd = reader(r);
        let mut buf = Vec::new();
        let (mut cur, mut in_t, mut rph, mut bytes) = (String::new(), false, 0u32, 0usize);
        loop {
            match rd.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match local(&e).as_str() {
                    "si" => cur.clear(),
                    "rPh" => rph += 1,
                    "t" if rph == 0 => in_t = true,
                    _ => {}
                },
                Ok(Event::End(e)) => match local_end(&e).as_str() {
                    "si" => {
                        bytes += cur.len();
                        strings.push(std::mem::take(&mut cur));
                        if bytes > MAX_SHARED_BYTES {
                            break;
                        }
                    }
                    "rPh" => rph = rph.saturating_sub(1),
                    "t" => in_t = false,
                    _ => {}
                },
                Ok(Event::Text(t)) if in_t => cur.push_str(&t.xml10_content()),
                Ok(Event::GeneralRef(g)) if in_t => {
                    if let Some(c) = entity(&g.into_inner()) {
                        cur.push(c);
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    });

    // Sheets in workbook order, via relationships; numbered files as the fallback.
    let rels = read_rels(z, "xl/_rels/workbook.xml.rels");
    let mut sheets: Vec<(String, String)> = Vec::new(); // (display name, part)
    for (i, (name, rid)) in list_refs(z, "xl/workbook.xml", "sheet").into_iter().enumerate() {
        if let Some(t) = rels.get(&rid) {
            sheets.push((name.unwrap_or_else(|| format!("Sheet{}", i + 1)), resolve_target("xl", t)));
        }
    }
    if sheets.is_empty() {
        let found = numbered(z.names_matching("xl/worksheets/sheet", ".xml"), "xl/worksheets/sheet");
        sheets = found.into_iter().enumerate().map(|(i, p)| (format!("Sheet{}", i + 1), p)).collect();
    }
    if sheets.is_empty() {
        return Err(ExtractError::Failed("not an xlsx (no worksheets)".into()));
    }
    for (name, part) in sheets {
        if out.full() {
            break;
        }
        out.section(Loc::Sheet(name.clone()), &format!("Sheet: {name}"));
        z.with_part(&part, |r| sheet_cells(r, &strings, out));
    }
    Ok(())
}

fn sheet_cells(r: &mut dyn BufRead, strings: &[String], out: &mut Out) {
    let mut rd = reader(r);
    let mut buf = Vec::new();
    let (mut ty, mut val) = (String::new(), String::new());
    let (mut in_v, mut in_t, mut row_has) = (false, false, false);
    while !out.full() {
        match rd.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local(&e).as_str() {
                "c" => {
                    ty = attr(&e, "t").unwrap_or_default();
                    val.clear();
                }
                "v" => in_v = true,
                "t" => in_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match local_end(&e).as_str() {
                "v" => in_v = false,
                "t" => in_t = false,
                "c" => {
                    let cell: Option<String> = match ty.as_str() {
                        "s" => val.trim().parse::<usize>().ok().and_then(|i| strings.get(i)).cloned(),
                        "e" => None,
                        "b" => Some(if val.trim() == "1" { "TRUE".into() } else { "FALSE".into() }),
                        _ => Some(std::mem::take(&mut val)),
                    };
                    if let Some(c) = cell.filter(|c| !c.trim().is_empty()) {
                        if row_has {
                            out.push_char('\t');
                        }
                        out.push(c.trim());
                        row_has = true;
                    }
                }
                "row" => {
                    if row_has {
                        out.push_char('\n');
                    }
                    row_has = false;
                }
                _ => {}
            },
            Ok(Event::Text(t)) if in_v || in_t => val.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(g)) if in_v || in_t => {
                if let Some(c) = entity(&g.into_inner()) {
                    val.push(c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if row_has {
        out.push_char('\n');
    }
}

fn pptx_text(z: &mut Zip<'_>, out: &mut Out) -> Res<()> {
    let rels = read_rels(z, "ppt/_rels/presentation.xml.rels");
    let mut slides: Vec<String> = list_refs(z, "ppt/presentation.xml", "sldId")
        .into_iter()
        .filter_map(|(_, rid)| rels.get(&rid).map(|t| resolve_target("ppt", t)))
        .collect();
    if slides.is_empty() {
        slides = numbered(z.names_matching("ppt/slides/slide", ".xml"), "ppt/slides/slide");
    }
    if slides.is_empty() {
        return Err(ExtractError::Failed("not a pptx (no slides)".into()));
    }
    for (i, part) in slides.iter().enumerate() {
        if out.full() {
            break;
        }
        let n = i as u32 + 1;
        out.section(Loc::Slide(n), &format!("Slide {n}"));
        z.with_part(part, |r| slide_text(r, out));
    }
    Ok(())
}

fn slide_text(r: &mut dyn BufRead, out: &mut Out) {
    let mut rd = reader(r);
    let mut buf = Vec::new();
    let mut in_t = false;
    while !out.full() {
        match rd.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local(&e).as_str() {
                "t" => in_t = true,
                "br" => out.push_char('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) if local(&e) == "br" => out.push_char('\n'),
            Ok(Event::End(e)) => match local_end(&e).as_str() {
                "t" => in_t = false,
                "p" if !out.ends_with_newline() => out.push_char('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_t => out.push(&t.xml10_content()),
            Ok(Event::GeneralRef(g)) if in_t => {
                if let Some(c) = entity(&g.into_inner()) {
                    out.push_char(c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

// ---------------------------------------------------------------- OpenDocument

#[derive(Clone, Copy, PartialEq, Eq)]
enum OdfKind {
    Text,
    Sheet,
    Slides,
}

fn odf(bytes: &[u8], kind: OdfKind) -> Res<Extracted> {
    let mut z = Zip::open(bytes)?;
    if !z.has("content.xml") {
        return Err(ExtractError::Failed("not an OpenDocument file (no content.xml)".into()));
    }
    let mut out = Out::new();
    z.with_part("content.xml", |r| odf_text(r, kind, &mut out));
    z.check()?;
    Ok(out.finish())
}

fn odf_text(r: &mut dyn BufRead, kind: OdfKind, out: &mut Out) {
    let mut rd = reader(r);
    let mut buf = Vec::new();
    let (mut para, mut cell_has, mut row_has) = (0u32, false, false);
    let mut slide = 0u32;
    while !out.full() {
        let ev = rd.read_event_into(&mut buf);
        let (open, empty) = match &ev {
            Ok(Event::Start(e)) => (Some(e.clone()), false),
            Ok(Event::Empty(e)) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = open {
            match local(&e).as_str() {
                "p" | "h" if !empty => para += 1,
                "tab" => out.push_char('\t'),
                "line-break" => out.push_char('\n'),
                "s" => {
                    let n = attr(&e, "c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1).min(64);
                    out.push(&" ".repeat(n));
                }
                "table" if kind == OdfKind::Sheet && !empty => {
                    let name = attr(&e, "name").unwrap_or_else(|| "Sheet".into());
                    out.section(Loc::Sheet(name.clone()), &format!("Sheet: {name}"));
                }
                "page" if kind == OdfKind::Slides && !empty => {
                    slide += 1;
                    out.section(Loc::Slide(slide), &format!("Slide {slide}"));
                }
                _ => {}
            }
        }
        match ev {
            Ok(Event::End(e)) => match local_end(&e).as_str() {
                "p" | "h" => {
                    para = para.saturating_sub(1);
                    if kind == OdfKind::Sheet {
                        cell_has = true;
                    } else {
                        out.push_char('\n');
                    }
                }
                "table-cell" if kind == OdfKind::Sheet => {
                    if cell_has {
                        out.push_char('\t');
                        row_has = true;
                    }
                    cell_has = false;
                }
                "table-row" if kind == OdfKind::Sheet => {
                    if row_has {
                        out.push_char('\n');
                    }
                    row_has = false;
                }
                _ => {}
            },
            Ok(Event::Text(t)) if para > 0 => {
                out.push(&t.xml10_content());
            }
            Ok(Event::GeneralRef(g)) if para > 0 => {
                if let Some(c) = entity(&g.into_inner()) {
                    out.push_char(c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build a zip with stored (uncompressed) entries.
    pub(crate) fn make_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in files {
            w.start_file(*name, opt).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    pub(crate) fn docx() -> Vec<u8> {
        make_zip(&[(
            "word/document.xml",
            r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
            <w:p><w:r><w:t>Quarterly report</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Profit &amp; loss </w:t></w:r><w:r><w:tab/><w:t>2024</w:t></w:r></w:p>
            <w:p><w:r><w:delText>removed</w:delText></w:r></w:p>
            </w:body></w:document>"#,
        )])
    }

    #[test]
    fn docx_paragraphs() {
        let d = extract_file("a.DOCX", &docx()).unwrap();
        assert_eq!(d.text, "Quarterly report\nProfit & loss \t2024\n\n");
        assert!(d.marks.is_empty());
    }

    pub(crate) fn xlsx() -> Vec<u8> {
        make_zip(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Budget" sheetId="1" r:id="rId2"/><sheet name="Notes" sheetId="2" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet2.xml"/><Relationship Id="rId2" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/sharedStrings.xml",
                r#"<sst><si><t>Rent</t></si><si><r><t>Elec</t></r><r><t>tricity</t></r><rPh><t>PHONETIC</t></rPh></si><si><t>Todo &lt;soon&gt;</t></si></sst>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1200</v></c></row>
                <row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2" t="inlineStr"><is><t>inline</t></is></c><c r="C2" t="e"><v>#DIV/0!</v></c></row></sheetData></worksheet>"#,
            ),
            ("xl/worksheets/sheet2.xml", r#"<worksheet><sheetData><row><c t="s"><v>2</v></c></row></sheetData></worksheet>"#),
        ])
    }

    #[test]
    fn xlsx_shared_strings_cells_and_sheet_names() {
        let d = extract_file("b.xlsx", &xlsx()).unwrap();
        assert_eq!(d.text, "Sheet: Budget\nRent\t1200\nElectricity\tinline\n\nSheet: Notes\nTodo <soon>\n");
        assert_eq!(d.marks.len(), 2);
        assert_eq!(d.marks[0], Mark { at: 0, loc: Loc::Sheet("Budget".into()) });
        assert_eq!(d.marks[1].loc, Loc::Sheet("Notes".into()));
        let at: String = d.text.chars().skip(d.marks[1].at).take(5).collect();
        assert_eq!(at, "Sheet");
    }

    pub(crate) fn pptx() -> Vec<u8> {
        make_zip(&[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId5"/><p:sldId id="257" r:id="rId4"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId4" Target="slides/slide1.xml"/><Relationship Id="rId5" Target="slides/slide2.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", r#"<p:sld><a:p><a:r><a:t>Second in order</a:t></a:r></a:p></p:sld>"#),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld><a:p><a:r><a:t>Welcome</a:t></a:r></a:p><a:p><a:r><a:t>Agenda</a:t></a:r><a:br/><a:r><a:t>Q&amp;A</a:t></a:r></a:p></p:sld>"#,
            ),
        ])
    }

    #[test]
    fn pptx_slides_in_presentation_order_with_markers() {
        let d = extract_file("c.pptx", &pptx()).unwrap();
        assert_eq!(d.text, "Slide 1\nWelcome\nAgenda\nQ&A\n\nSlide 2\nSecond in order\n");
        assert_eq!(d.marks.iter().map(|m| m.loc.clone()).collect::<Vec<_>>(), [Loc::Slide(1), Loc::Slide(2)]);
    }

    #[test]
    fn pptx_without_presentation_part_falls_back_to_numbered_slides() {
        let z = make_zip(&[
            ("ppt/slides/slide10.xml", "<s><a:p><a:t>ten</a:t></a:p></s>"),
            ("ppt/slides/slide2.xml", "<s><a:p><a:t>two</a:t></a:p></s>"),
        ]);
        let d = extract_file("x.pptx", &z).unwrap();
        assert_eq!(d.text, "Slide 1\ntwo\n\nSlide 2\nten\n");
    }

    #[test]
    fn odt_ods_odp() {
        let odt = make_zip(&[(
            "content.xml",
            r#"<office:document-content xmlns:text="t"><office:body><office:text><text:h>Title</text:h><text:p>Hello<text:s text:c="2"/>world<text:tab/>x<text:line-break/>y &amp; z</text:p></office:text></office:body></office:document-content>"#,
        )]);
        assert_eq!(extract_file("a.odt", &odt).unwrap().text, "Title\nHello  world\tx\ny & z\n");

        let ods = make_zip(&[(
            "content.xml",
            r#"<c><table:table table:name="Data"><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell/><table:table-cell><text:p>b</text:p></table:table-cell></table:table-row></table:table></c>"#,
        )]);
        let d = extract_file("a.ods", &ods).unwrap();
        assert_eq!(d.text, "Sheet: Data\na\tb\t\n");
        assert_eq!(d.marks[0].loc, Loc::Sheet("Data".into()));

        let odp = make_zip(&[(
            "content.xml",
            r#"<c><draw:page draw:name="p1"><text:p>one</text:p></draw:page><draw:page><text:p>two</text:p></draw:page></c>"#,
        )]);
        assert_eq!(extract_file("a.odp", &odp).unwrap().text, "Slide 1\none\n\nSlide 2\ntwo\n");
    }

    #[test]
    fn legacy_office_and_ole_are_skipped() {
        for n in ["a.doc", "a.XLS", "a.ppt"] {
            assert_eq!(extract_file(n, b"whatever"), Err(ExtractError::Skip("legacy_office")), "{n}");
        }
        let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        assert_eq!(extract_file("enc.docx", &ole), Err(ExtractError::Skip("legacy_office")));
    }

    /// The preview's document list must name exactly the extensions that run a structured reader:
    /// garbage bytes reach the zip readers (Failed), not the plain-text fallback, and RTF decodes.
    #[test]
    fn document_exts_are_the_structured_readers() {
        for ext in DOCUMENT_EXTS {
            if *ext == "rtf" {
                assert_eq!(extract_file(&format!("f.{ext}"), b"{\\rtf1 hi}").unwrap().text, "hi", "{ext}");
                continue;
            }
            assert!(matches!(extract_file(&format!("f.{ext}"), b"garbage bytes"), Err(ExtractError::Failed(_))), "{ext}");
        }
        assert!(is_document_ext("pptx"));
        assert!(!is_document_ext("PPTX"), "the caller lower-cases first");
        assert!(!is_document_ext("doc"), "legacy Office is not previewable as text");
        assert!(!is_document_ext("txt"));
    }

    #[test]
    fn corrupt_and_wrong_zips() {
        assert!(matches!(extract_file("a.docx", b"not a zip at all"), Err(ExtractError::Failed(_))));
        let z = make_zip(&[("readme.txt", "hi")]);
        assert!(matches!(extract_file("a.docx", &z), Err(ExtractError::Failed(_))));
        assert!(matches!(extract_file("a.xlsx", &z), Err(ExtractError::Failed(_))));
        assert!(matches!(extract_file("a.odt", &z), Err(ExtractError::Failed(_))));
        // empty document
        let empty = make_zip(&[("word/document.xml", "<w:document><w:body/></w:document>")]);
        assert_eq!(extract_file("e.docx", &empty), Err(ExtractError::Skip("no_text")));
    }

    #[test]
    fn text_cap_applies_to_office_text() {
        let para = "<w:p><w:r><w:t>0123456789abcdef0123456789abcdef</w:t></w:r></w:p>".repeat(20_000);
        let z = make_zip(&[("word/document.xml", &format!("<w:document><w:body>{para}</w:body></w:document>"))]);
        let d = extract_file("big.docx", &z).unwrap();
        assert!(d.truncated);
        assert_eq!(d.text.len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn zip_bomb_is_skipped() {
        // ~160 MB of inflated XML (highly compressible) in one part: exceeds the budget.
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("word/document.xml", opt).unwrap();
        w.write_all(b"<w:document><w:body>").unwrap();
        let block = vec![b' '; 1024 * 1024];
        for _ in 0..(MAX_PART / (1024 * 1024) + 40) {
            w.write_all(&block).unwrap();
        }
        w.write_all(b"<w:p><w:t>late text</w:t></w:p></w:body></w:document>").unwrap();
        let bytes = w.finish().unwrap().into_inner();
        assert!(bytes.len() < 1024 * 1024, "the bomb itself is small: {}", bytes.len());
        assert_eq!(extract_file("bomb.docx", &bytes), Err(ExtractError::Skip("zip_bomb")));
    }

    #[test]
    fn plain_text_encodings_and_binary() {
        assert_eq!(extract_file("a.md", b"# Title\r\nbody\r\n").unwrap().text, "# Title\nbody\n");
        assert_eq!(extract_file("a.txt", b"\xFF\xFEh\0i\0").unwrap().text, "hi");
        assert_eq!(extract_file("a.csv", b"caf\xE9,1").unwrap().text, "caf\u{e9},1");
        assert_eq!(extract_file("a.log", b"\x00\x01\x02binary"), Err(ExtractError::Skip("binary")));
        assert_eq!(extract_file("a.txt", b"  \n \n"), Err(ExtractError::Skip("no_text")));
        // only a prefix of a huge file is decoded
        let big = vec![b'a'; TEXT_READ_CAP + 1000];
        assert_eq!(extract_file("big.txt", &big).unwrap().text.len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn rtf_stripping() {
        let rtf = r"{\rtf1\ansi\deff0{\fonttbl{\f0 Arial;}}{\colortbl;\red0\green0\blue0;}{\*\generator Word;}\pard Hello \b bold\b0  world\par Caf\'e9 \u8364?x\tab y\par {\info{\title Hidden}}\{braces\}\par}";
        let t = strip_rtf(rtf);
        assert_eq!(t, "Hello bold world\nCaf\u{e9} \u{20ac}x\ty\n{braces}\n");
        let d = extract_file("a.rtf", rtf.as_bytes()).unwrap();
        assert!(d.text.starts_with("Hello bold world"));
        assert!(!d.text.contains("Arial") && !d.text.contains("Hidden") && !d.text.contains("generator"));
        assert_eq!(strip_rtf(r"{\rtf1 \u-4000?z}"), "\u{f060}z");
    }
}
