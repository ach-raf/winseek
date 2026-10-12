//! Pure parser for the `FSCTL_READ_USN_JOURNAL` output buffer and reason
//! classification. No Win32.

use std::fmt;

pub const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const USN_REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub const USN_REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub const USN_REASON_CLOSE: u32 = 0x8000_0000;

/// Fixed part of USN_RECORD_V2 (the name starts at FileNameOffset >= 60).
const V2_HEADER: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub usn: i64,
    pub reason: u32,
    pub attrs: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsnError {
    /// Buffer shorter than the 8-byte next-USN header, or a record header is cut off.
    Truncated { offset: usize },
    /// RecordLength is < 8, not 8-aligned, or runs past the buffer.
    BadRecordLength { offset: usize, len: u32 },
    /// A V2 record whose FileNameOffset/Length fall outside the record or are odd.
    BadName { offset: usize },
}

impl fmt::Display for UsnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsnError::Truncated { offset } => write!(f, "USN buffer truncated at {offset}"),
            UsnError::BadRecordLength { offset, len } => {
                write!(f, "bad USN RecordLength {len} at {offset}")
            }
            UsnError::BadName { offset } => write!(f, "bad USN file name bounds at {offset}"),
        }
    }
}

impl std::error::Error for UsnError {}

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}

/// Parse a `FSCTL_READ_USN_JOURNAL` output buffer: the first 8 bytes are the
/// next USN to read from, followed by `USN_RECORD_V2` entries.
///
/// Records of any other major version (V3 128-bit FRNs, V4 range records) are
/// skipped using their RecordLength. The first structurally invalid record
/// aborts the parse with an error (the caller should treat that as a read
/// error / resync point); this function never panics.
pub fn parse_usn_buffer(buf: &[u8]) -> Result<(i64, Vec<UsnRecord>), UsnError> {
    if buf.len() < 8 {
        return Err(UsnError::Truncated { offset: 0 });
    }
    let next_usn = rd_u64(buf, 0) as i64;
    let mut records = Vec::new();
    let mut off = 8usize;
    while off < buf.len() {
        let remaining = buf.len() - off;
        if remaining < 8 {
            return Err(UsnError::Truncated { offset: off });
        }
        let len = rd_u32(buf, off);
        let l = len as usize;
        if l < 8 || !l.is_multiple_of(8) || l > remaining {
            return Err(UsnError::BadRecordLength { offset: off, len });
        }
        let major = rd_u16(buf, off + 4);
        if major == 2 {
            if l < V2_HEADER {
                return Err(UsnError::BadRecordLength { offset: off, len });
            }
            let rec = &buf[off..off + l];
            let name_len = rd_u16(rec, 56) as usize;
            let name_off = rd_u16(rec, 58) as usize;
            if name_off < V2_HEADER || !name_len.is_multiple_of(2) || name_off + name_len > l {
                return Err(UsnError::BadName { offset: off });
            }
            let units: Vec<u16> = rec[name_off..name_off + name_len]
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            records.push(UsnRecord {
                frn: rd_u64(rec, 8),
                parent_frn: rd_u64(rec, 16),
                usn: rd_u64(rec, 24) as i64,
                reason: rd_u32(rec, 40),
                attrs: rd_u32(rec, 52),
                name: String::from_utf16_lossy(&units),
            });
        }
        off += l;
    }
    Ok((next_usn, records))
}

/// What a USN record means for the name index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Create,
    Delete,
    RenameOld,
    RenameNew,
    /// Data change (overwrite/extend/truncate) or basic-info change (size,
    /// timestamps, attributes).
    Modify,
    Other,
}

/// Classify a (possibly multi-bit) reason mask.
///
/// Precedence: Delete > RenameOld > RenameNew > Create > Modify > Other.
/// Refinement: the journal accumulates reason bits for the lifetime of an
/// open handle, so the RENAME_NEW record can also carry RENAME_OLD_NAME. When
/// both rename bits are set the record is the later (new-name) record and is
/// classified `RenameNew`; `RenameOld` means the old bit WITHOUT the new bit.
pub fn classify(reason: u32) -> Op {
    let old = reason & USN_REASON_RENAME_OLD_NAME != 0;
    let new = reason & USN_REASON_RENAME_NEW_NAME != 0;
    if reason & USN_REASON_FILE_DELETE != 0 {
        Op::Delete
    } else if old && !new {
        Op::RenameOld
    } else if new {
        Op::RenameNew
    } else if reason & USN_REASON_FILE_CREATE != 0 {
        Op::Create
    } else if reason
        & (USN_REASON_DATA_OVERWRITE
            | USN_REASON_DATA_EXTEND
            | USN_REASON_DATA_TRUNCATION
            | USN_REASON_BASIC_INFO_CHANGE)
        != 0
    {
        Op::Modify
    } else {
        Op::Other
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build one USN_RECORD_V2.
    pub fn v2(frn: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
        let u: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let len = (V2_HEADER + u.len() + 7) & !7;
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[24..32].copy_from_slice(&usn.to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(u.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&(V2_HEADER as u16).to_le_bytes());
        r[V2_HEADER..V2_HEADER + u.len()].copy_from_slice(&u);
        r
    }

    pub fn buffer(next: i64, recs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = next.to_le_bytes().to_vec();
        for r in recs {
            b.extend_from_slice(r);
        }
        b
    }

    #[test]
    fn empty_buffer_is_just_next_usn() {
        let (n, r) = parse_usn_buffer(&buffer(4242, &[])).unwrap();
        assert_eq!(n, 4242);
        assert!(r.is_empty());
    }

    #[test]
    fn parses_multiple_records_with_unicode() {
        let b = buffer(
            900,
            &[
                v2(10, 5, 100, USN_REASON_FILE_CREATE, 0x20, "a.txt"),
                v2(11 | (3 << 48), 5, 200, USN_REASON_FILE_DELETE | USN_REASON_CLOSE, 0x10, "日本語"),
            ],
        );
        let (n, r) = parse_usn_buffer(&b).unwrap();
        assert_eq!(n, 900);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], UsnRecord { frn: 10, parent_frn: 5, usn: 100, reason: 0x100, attrs: 0x20, name: "a.txt".into() });
        assert_eq!(r[1].frn, 11 | (3 << 48));
        assert_eq!(r[1].name, "日本語");
        assert_eq!(r[1].attrs, 0x10);
    }

    #[test]
    fn skips_v3_and_v4_records() {
        let mut v3 = vec![0u8; 80];
        v3[0..4].copy_from_slice(&80u32.to_le_bytes());
        v3[4..6].copy_from_slice(&3u16.to_le_bytes());
        let mut v4 = vec![0u8; 32];
        v4[0..4].copy_from_slice(&32u32.to_le_bytes());
        v4[4..6].copy_from_slice(&4u16.to_le_bytes());
        let b = buffer(1, &[v3, v2(1, 2, 3, 0x100, 0, "x"), v4]);
        let (_, r) = parse_usn_buffer(&b).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "x");
    }

    #[test]
    fn rejects_short_buffer() {
        assert_eq!(parse_usn_buffer(&[1, 2, 3]), Err(UsnError::Truncated { offset: 0 }));
    }

    #[test]
    fn rejects_truncated_record_header() {
        let mut b = buffer(1, &[]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(parse_usn_buffer(&b), Err(UsnError::Truncated { offset: 8 }));
    }

    #[test]
    fn rejects_bad_record_lengths() {
        for bad in [0u32, 4, 61, 8 * 1000] {
            let mut r = v2(1, 2, 3, 0, 0, "n");
            r[0..4].copy_from_slice(&bad.to_le_bytes());
            let b = buffer(1, &[r]);
            assert!(matches!(parse_usn_buffer(&b), Err(UsnError::BadRecordLength { .. })), "len {bad}");
        }
    }

    #[test]
    fn v2_shorter_than_header_is_rejected() {
        let mut r = vec![0u8; 16];
        r[0..4].copy_from_slice(&16u32.to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(parse_usn_buffer(&buffer(1, &[r])), Err(UsnError::BadRecordLength { .. })));
    }

    #[test]
    fn rejects_bad_name_bounds() {
        // offset below header
        let mut r = v2(1, 2, 3, 0, 0, "name");
        r[58..60].copy_from_slice(&8u16.to_le_bytes());
        assert!(matches!(parse_usn_buffer(&buffer(1, &[r])), Err(UsnError::BadName { .. })));
        // length past record end
        let mut r = v2(1, 2, 3, 0, 0, "name");
        r[56..58].copy_from_slice(&500u16.to_le_bytes());
        assert!(matches!(parse_usn_buffer(&buffer(1, &[r])), Err(UsnError::BadName { .. })));
        // odd length
        let mut r = v2(1, 2, 3, 0, 0, "name");
        r[56..58].copy_from_slice(&3u16.to_le_bytes());
        assert!(matches!(parse_usn_buffer(&buffer(1, &[r])), Err(UsnError::BadName { .. })));
        // offset+length overflow of u16 arithmetic must not wrap
        let mut r = v2(1, 2, 3, 0, 0, "name");
        r[56..58].copy_from_slice(&0xFFFEu16.to_le_bytes());
        r[58..60].copy_from_slice(&0xFFFEu16.to_le_bytes());
        assert!(matches!(parse_usn_buffer(&buffer(1, &[r])), Err(UsnError::BadName { .. })));
    }

    #[test]
    fn fuzz_prefixes_never_panic() {
        let b = buffer(7, &[v2(1, 2, 3, 0x100, 0, "hello"), v2(4, 5, 6, 0x200, 0, "world")]);
        for cut in 0..b.len() {
            let _ = parse_usn_buffer(&b[..cut]);
        }
        let mut c = b.clone();
        for i in 8..c.len() {
            let old = c[i];
            c[i] = old ^ 0xFF;
            let _ = parse_usn_buffer(&c);
            c[i] = old;
        }
    }

    #[test]
    fn classify_precedence() {
        use super::USN_REASON_CLOSE as CL;
        assert_eq!(classify(USN_REASON_FILE_CREATE | USN_REASON_FILE_DELETE), Op::Delete);
        assert_eq!(classify(USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME), Op::Delete);
        assert_eq!(classify(USN_REASON_RENAME_OLD_NAME | USN_REASON_FILE_CREATE), Op::RenameOld);
        assert_eq!(classify(USN_REASON_RENAME_NEW_NAME | USN_REASON_FILE_CREATE), Op::RenameNew);
        // accumulated bits on the new-name record
        assert_eq!(classify(USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME), Op::RenameNew);
        assert_eq!(classify(USN_REASON_FILE_CREATE | USN_REASON_DATA_EXTEND | CL), Op::Create);
        assert_eq!(classify(USN_REASON_DATA_EXTEND), Op::Modify);
        assert_eq!(classify(USN_REASON_DATA_OVERWRITE | CL), Op::Modify);
        assert_eq!(classify(USN_REASON_BASIC_INFO_CHANGE), Op::Modify);
        assert_eq!(classify(CL), Op::Other);
        assert_eq!(classify(0x0000_0800), Op::Other); // security change
    }
}
