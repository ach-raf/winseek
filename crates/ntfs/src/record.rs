//! Pure MFT FILE-record decoding (no Win32). Ported from wiztree-rs
//! `record.rs`, reshaped: panic-free accessors, fixup validation, and every
//! `$FILE_NAME` is retained so hard links can be emitted.

pub const ATTR_STD_INFO: u32 = 0x10;
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
/// 48-bit MFT record number (sequence number stripped).
pub const REC_NO_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// Update-sequence stride inside MFT records is always 512 bytes.
pub const SECTOR: usize = 512;

/// Little-endian reads that return 0 when out of bounds, so a corrupt record
/// can never panic (loops terminate on the zero lengths).
pub fn u16_at(b: &[u8], off: usize) -> u16 {
    b.get(off..off + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
pub fn u64_at(b: &[u8], off: usize) -> u64 {
    b.get(off..off + 8).map_or(0, |s| {
        let mut v = [0u8; 8];
        v.copy_from_slice(s);
        u64::from_le_bytes(v)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// No `FILE` signature (unused / zeroed / `BAAD` slot).
    BadMagic,
    /// Record is not flagged in-use.
    NotInUse,
    /// USA header out of bounds.
    BadFixup,
    /// A sector tail did not match the update sequence number (torn write).
    FixupMismatch,
}

/// One `$FILE_NAME` attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameAttr {
    /// Full 64-bit parent file reference (record number + sequence).
    pub parent_ref: u64,
    /// 0 POSIX, 1 Win32, 2 DOS, 3 Win32&DOS.
    pub namespace: u8,
    pub name: String,
}

/// Everything we need from one in-use record.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub recno: u32,
    pub seq: u16,
    pub is_dir: bool,
    /// Header +0x20 (masked to 48 bits): 0 for base records, else the base
    /// record this extension record belongs to.
    pub base_ref: u32,
    pub has_attr_list: bool,
    pub names: Vec<NameAttr>,
    /// ($STANDARD_INFORMATION) modified FILETIME and DOS attribute bits.
    pub std: Option<(u64, u32)>,
    /// Logical size of the unnamed $DATA (None if no unnamed $DATA seen).
    pub size: Option<u64>,
    /// Allocated bytes: mapped clusters of unnamed non-resident $DATA runs.
    pub alloc: u64,
}

impl Parsed {
    /// Fold an extension record's attributes into this base record.
    pub fn merge(&mut self, ext: Parsed) {
        self.names.extend(ext.names);
        if self.std.is_none() {
            self.std = ext.std;
        }
        if self.size.is_none() {
            self.size = ext.size;
        }
        self.alloc += ext.alloc;
    }

    /// FRN of this (base) record as seen in USN records.
    pub fn frn(&self) -> u64 {
        (self.recno as u64) | ((self.seq as u64) << 48)
    }
}

/// Undo the update-sequence array in place, validating each sector tail.
pub fn apply_fixup(rec: &mut [u8]) -> Result<(), RecordError> {
    let usa_off = u16_at(rec, 4) as usize;
    let usa_count = u16_at(rec, 6) as usize;
    if usa_count < 1 || usa_off + 2 * usa_count > rec.len() || (usa_count - 1) * SECTOR > rec.len() {
        return Err(RecordError::BadFixup);
    }
    let usn = u16_at(rec, usa_off);
    for i in 1..usa_count {
        let pos = i * SECTOR - 2;
        if u16_at(rec, pos) != usn {
            return Err(RecordError::FixupMismatch);
        }
        let orig = [rec[usa_off + 2 * i], rec[usa_off + 2 * i + 1]];
        rec[pos..pos + 2].copy_from_slice(&orig);
    }
    Ok(())
}

/// Decode an NTFS data-run list. Returns (clusters, Some(lcn)) per mapped
/// run and (clusters, None) per sparse run. Stops on malformed input.
pub fn decode_data_runs(buf: &[u8], runs_off: usize, max_len: usize) -> Vec<(u64, Option<i64>)> {
    let max_len = max_len.min(buf.len());
    let mut runs = Vec::new();
    let mut pos = runs_off;
    let mut lcn: i64 = 0;
    while pos < max_len {
        let hdr = buf[pos];
        if hdr == 0 {
            break;
        }
        let len_f = (hdr & 0xF) as usize;
        let off_f = (hdr >> 4) as usize;
        pos += 1;
        if len_f == 0 || len_f > 8 || off_f > 8 || pos + len_f + off_f > max_len {
            break;
        }
        let mut clusters: u64 = 0;
        for (i, b) in buf[pos..pos + len_f].iter().enumerate() {
            clusters |= (*b as u64) << (8 * i);
        }
        pos += len_f;
        if off_f > 0 {
            let mut delta: i64 = buf[pos + off_f - 1] as i8 as i64;
            for i in (0..off_f - 1).rev() {
                delta = (delta << 8) | buf[pos + i] as i64;
            }
            pos += off_f;
            lcn = lcn.wrapping_add(delta);
            runs.push((clusters, Some(lcn)));
        } else {
            runs.push((clusters, None));
        }
    }
    runs
}

/// UTF-16LE bytes -> String (lossy, same result as `String::from_utf16_lossy`)
/// without an intermediate `Vec<u16>`; ASCII-only names take a fast path.
fn utf16le_to_string(b: &[u8]) -> String {
    let n = b.len() / 2;
    let mut s = String::with_capacity(n);
    let mut it = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]]));
    // Fast path: copy ASCII units until the first non-ASCII one.
    let mut rest = None;
    for u in it.by_ref() {
        if u < 0x80 {
            s.push(u as u8 as char);
        } else {
            rest = Some(u);
            break;
        }
    }
    if let Some(first) = rest {
        let tail = std::iter::once(first).chain(it);
        s.extend(char::decode_utf16(tail).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
    }
    s
}

/// Parse one MFT record in place (fixup mutates `rec`).
pub fn parse_record(rec: &mut [u8], recno: u32, cluster: u64) -> Result<Parsed, RecordError> {
    if rec.len() < 0x30 || &rec[0..4] != b"FILE" {
        return Err(RecordError::BadMagic);
    }
    apply_fixup(rec)?;
    // Flags live at 0x16 (NOT 0x1C, which is the allocated size).
    let flags = u16_at(rec, 0x16);
    if flags & 0x01 == 0 {
        return Err(RecordError::NotInUse);
    }
    let mut p = Parsed {
        recno,
        seq: u16_at(rec, 0x10),
        is_dir: flags & 0x02 != 0,
        base_ref: (u64_at(rec, 0x20) & REC_NO_MASK) as u32,
        names: Vec::with_capacity(2),
        ..Default::default()
    };

    let mut off = u16_at(rec, 0x14) as usize;
    let mut used = u32_at(rec, 0x18) as usize;
    if used == 0 || used > rec.len() {
        used = rec.len();
    }
    while off + 0x18 <= used {
        let ty = u32_at(rec, off);
        if ty == 0xFFFF_FFFF || ty == 0 {
            break;
        }
        let alen = u32_at(rec, off + 4) as usize;
        if alen < 0x18 || off + alen > used {
            break;
        }
        let end = off + alen;
        let non_resident = rec[off + 8];
        let attr_name_len = rec[off + 9];
        match ty {
            ATTR_STD_INFO if non_resident == 0 => {
                let v = off + u16_at(rec, off + 0x14) as usize;
                if v + 0x24 <= end {
                    p.std = Some((u64_at(rec, v + 0x08), u32_at(rec, v + 0x20)));
                }
            }
            ATTR_FILE_NAME if non_resident == 0 && attr_name_len == 0 => {
                let v = off + u16_at(rec, off + 0x14) as usize;
                if v + 0x42 <= end {
                    let nm_len = rec[v + 0x40] as usize;
                    if v + 0x42 + nm_len * 2 <= end {
                        let name = utf16le_to_string(&rec[v + 0x42..v + 0x42 + nm_len * 2]);
                        p.names.push(NameAttr {
                            parent_ref: u64_at(rec, v),
                            namespace: rec[v + 0x41],
                            name,
                        });
                    }
                }
            }
            ATTR_ATTRIBUTE_LIST => p.has_attr_list = true,
            ATTR_DATA if attr_name_len == 0 => {
                if non_resident != 0 {
                    let lowest_vcn = u64_at(rec, off + 0x10);
                    let runs_off = off + u16_at(rec, off + 0x20) as usize;
                    let clusters: u64 = decode_data_runs(rec, runs_off, end)
                        .iter()
                        .filter(|(_, lcn)| lcn.is_some())
                        .map(|(c, _)| *c)
                        .sum();
                    p.alloc = p.alloc.saturating_add(clusters.saturating_mul(cluster));
                    // Only the first extent (LowestVCN 0) carries the real size.
                    if lowest_vcn == 0 {
                        p.size = Some(u64_at(rec, off + 0x30));
                    }
                } else {
                    p.size = Some(u32_at(rec, off + 0x10) as u64);
                }
            }
            _ => {}
        }
        off = end;
    }
    Ok(p)
}

/// Choose which names to emit. DOS (namespace 2) names are dropped when a
/// non-DOS name with the same parent exists; otherwise they are the fallback.
/// Distinct (parent, name) pairs are all kept (hard links); exact duplicates
/// collapse.
pub fn select_names(names: &[NameAttr]) -> Vec<&NameAttr> {
    let mut out: Vec<&NameAttr> = Vec::new();
    for n in names {
        if n.namespace == 2
            && names
                .iter()
                .any(|o| o.namespace != 2 && o.parent_ref & REC_NO_MASK == n.parent_ref & REC_NO_MASK)
        {
            continue;
        }
        if out
            .iter()
            .any(|o| o.parent_ref == n.parent_ref && o.name == n.name)
        {
            continue;
        }
        out.push(n);
    }
    out
}

/// Synthetic-record builder shared by unit tests, the offline decode benchmark
/// and the bench example (`testutil` feature). Not part of the stable API.
/// Owned variant of [`select_names`] (same semantics) that moves the names out
/// instead of cloning them.
pub fn select_names_owned(mut names: Vec<NameAttr>) -> Vec<NameAttr> {
    if names.len() <= 1 {
        return names;
    }
    if names.len() == 2 {
        // Common case (Win32 + DOS alias, or two hard links): no extra allocation.
        let (a, b) = (&names[0], &names[1]);
        let same_parent = a.parent_ref & REC_NO_MASK == b.parent_ref & REC_NO_MASK;
        if same_parent && b.namespace == 2 && a.namespace != 2 {
            names.truncate(1);
        } else if same_parent && a.namespace == 2 && b.namespace != 2 {
            names.swap_remove(0);
        } else if a.parent_ref == b.parent_ref && a.name == b.name {
            names.truncate(1);
        }
        return names;
    }
    let mut keep: Vec<usize> = Vec::with_capacity(names.len());
    for (i, n) in names.iter().enumerate() {
        if n.namespace == 2
            && names
                .iter()
                .any(|o| o.namespace != 2 && o.parent_ref & REC_NO_MASK == n.parent_ref & REC_NO_MASK)
        {
            continue;
        }
        if keep
            .iter()
            .any(|&k| names[k].parent_ref == n.parent_ref && names[k].name == n.name)
        {
            continue;
        }
        keep.push(i);
    }
    keep.iter().map(|&i| std::mem::take(&mut names[i])).collect()
}

#[cfg(any(test, feature = "testutil"))]
#[doc(hidden)]
pub mod testutil {
    use super::*;

    pub struct Rb {
        pub rec: Vec<u8>,
        pub off: usize,
    }

    /// Synthetic 1024-byte record with a 3-entry USA (usn = 0x1234 not yet
    /// applied to sector tails; call `seal` to emulate the on-disk form).
    impl Rb {
        pub fn new(in_use: bool, is_dir: bool) -> Rb {
            let mut rec = vec![0u8; 1024];
            rec[0..4].copy_from_slice(b"FILE");
            rec[4..6].copy_from_slice(&0x30u16.to_le_bytes());
            rec[6..8].copy_from_slice(&3u16.to_le_bytes());
            rec[0x10..0x12].copy_from_slice(&7u16.to_le_bytes()); // sequence
            rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
            rec[0x16..0x18]
                .copy_from_slice(&((in_use as u16) | ((is_dir as u16) << 1)).to_le_bytes());
            rec[0x18..0x1C].copy_from_slice(&1024u32.to_le_bytes());
            Rb { rec, off: 0x38 }
        }
        fn hdr(&mut self, ty: u32, len: usize) -> usize {
            let o = self.off;
            self.rec[o..o + 4].copy_from_slice(&ty.to_le_bytes());
            self.rec[o + 4..o + 8].copy_from_slice(&(len as u32).to_le_bytes());
            self.off += len;
            o
        }
        pub fn name(mut self, parent: u64, name: &str, ns: u8) -> Self {
            let u: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            let len = (0x18 + 0x42 + u.len() + 7) & !7;
            let o = self.hdr(ATTR_FILE_NAME, len);
            self.rec[o + 0x10..o + 0x14].copy_from_slice(&((0x42 + u.len()) as u32).to_le_bytes());
            self.rec[o + 0x14..o + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
            let v = o + 0x18;
            self.rec[v..v + 8].copy_from_slice(&parent.to_le_bytes());
            self.rec[v + 0x40] = name.encode_utf16().count() as u8;
            self.rec[v + 0x41] = ns;
            self.rec[v + 0x42..v + 0x42 + u.len()].copy_from_slice(&u);
            self
        }
        pub fn std_info(mut self, mtime: u64, attrs: u32) -> Self {
            let o = self.hdr(ATTR_STD_INFO, 0x18 + 0x30);
            self.rec[o + 0x10..o + 0x14].copy_from_slice(&0x30u32.to_le_bytes());
            self.rec[o + 0x14..o + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
            let v = o + 0x18;
            self.rec[v + 8..v + 16].copy_from_slice(&mtime.to_le_bytes());
            self.rec[v + 0x20..v + 0x24].copy_from_slice(&attrs.to_le_bytes());
            self
        }
        pub fn data_resident(mut self, size: u32) -> Self {
            let o = self.hdr(ATTR_DATA, 0x18);
            self.rec[o + 0x10..o + 0x14].copy_from_slice(&size.to_le_bytes());
            self.rec[o + 0x14..o + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
            self
        }
        /// non-resident unnamed $DATA: one run of `clusters` at LCN 100.
        pub fn data_nonres(mut self, lowest_vcn: u64, size: u64, clusters: u8) -> Self {
            let o = self.hdr(ATTR_DATA, 0x48);
            self.rec[o + 8] = 1;
            self.rec[o + 0x10..o + 0x18].copy_from_slice(&lowest_vcn.to_le_bytes());
            self.rec[o + 0x20..o + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
            self.rec[o + 0x30..o + 0x38].copy_from_slice(&size.to_le_bytes());
            self.rec[o + 0x40..o + 0x44].copy_from_slice(&[0x11, clusters, 100, 0]);
            self
        }
        pub fn attr_list(mut self) -> Self {
            let o = self.hdr(ATTR_ATTRIBUTE_LIST, 0x18);
            self.rec[o + 0x14..o + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
            self
        }
        pub fn base(mut self, base: u64) -> Self {
            self.rec[0x20..0x28].copy_from_slice(&base.to_le_bytes());
            self
        }
        /// Emulate on-disk form: sector tails hold the USN, USA holds originals.
        pub fn seal(mut self) -> Vec<u8> {
            let end = self.off;
            self.rec[end..end + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            let usa = 0x30;
            self.rec[usa..usa + 2].copy_from_slice(&0x1234u16.to_le_bytes());
            for i in 1..3 {
                let pos = i * 512 - 2;
                let (a, b) = (self.rec[pos], self.rec[pos + 1]);
                self.rec[usa + 2 * i] = a;
                self.rec[usa + 2 * i + 1] = b;
                self.rec[pos..pos + 2].copy_from_slice(&0x1234u16.to_le_bytes());
            }
            self.rec
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::Rb;
    use super::*;

    const CL: u64 = 4096;

    #[test]
    fn basic_file_with_fixup() {
        let mut rec = Rb::new(true, false)
            .std_info(1234, 0x20)
            .name(5 | (5 << 48), "hello.txt", 1)
            .data_resident(99)
            .seal();
        // plant distinctive original bytes under the USA tails
        rec[510..512].copy_from_slice(&0x1234u16.to_le_bytes());
        let p = parse_record(&mut rec, 100, CL).unwrap();
        assert_eq!(p.recno, 100);
        assert_eq!(p.frn(), 100 | (7u64 << 48));
        assert_eq!(p.names.len(), 1);
        assert_eq!(p.names[0].name, "hello.txt");
        assert_eq!(p.names[0].parent_ref, 5 | (5 << 48));
        assert_eq!(p.std, Some((1234, 0x20)));
        assert_eq!(p.size, Some(99));
        assert_eq!(p.alloc, 0);
        assert!(!p.is_dir);
        assert_eq!(p.base_ref, 0);
    }

    #[test]
    fn fixup_restores_sector_tails() {
        let mut b = Rb::new(true, false).name(5, "f", 1);
        b.rec[510..512].copy_from_slice(&0xCAFEu16.to_le_bytes());
        b.rec[1022..1024].copy_from_slice(&0xBEEFu16.to_le_bytes());
        let mut rec = b.seal();
        assert_eq!(&rec[510..512], &0x1234u16.to_le_bytes());
        parse_record(&mut rec, 1, CL).unwrap();
        assert_eq!(&rec[510..512], &0xCAFEu16.to_le_bytes());
        assert_eq!(&rec[1022..1024], &0xBEEFu16.to_le_bytes());
    }

    #[test]
    fn torn_record_is_rejected() {
        let mut rec = Rb::new(true, false).name(5, "f", 1).seal();
        rec[1022] ^= 0xFF; // tail no longer equals USN
        assert_eq!(parse_record(&mut rec, 1, CL).unwrap_err(), RecordError::FixupMismatch);
    }

    #[test]
    fn bad_usa_bounds() {
        let mut rec = Rb::new(true, false).seal();
        rec[6..8].copy_from_slice(&200u16.to_le_bytes());
        assert_eq!(parse_record(&mut rec, 1, CL).unwrap_err(), RecordError::BadFixup);
    }

    #[test]
    fn magic_and_in_use() {
        let mut z = vec![0u8; 1024];
        assert_eq!(parse_record(&mut z, 0, CL).unwrap_err(), RecordError::BadMagic);
        let mut d = Rb::new(false, false).name(5, "gone", 1).seal();
        assert_eq!(parse_record(&mut d, 0, CL).unwrap_err(), RecordError::NotInUse);
    }

    #[test]
    fn nonresident_alloc_from_runs_and_dir_flag() {
        let mut rec = Rb::new(true, true).name(5, "d", 1).data_nonres(0, 5_000_000, 200).seal();
        let p = parse_record(&mut rec, 7, CL).unwrap();
        assert!(p.is_dir);
        assert_eq!(p.size, Some(5_000_000));
        assert_eq!(p.alloc, 200 * CL);
    }

    #[test]
    fn extension_record_and_merge() {
        let mut base = Rb::new(true, false).name(5, "big.bin", 1).attr_list().seal();
        let mut ext = Rb::new(true, false).base(77 | (3 << 48)).data_nonres(0, 10_000, 3).seal();
        let mut bp = parse_record(&mut base, 77, CL).unwrap();
        let ep = parse_record(&mut ext, 500, CL).unwrap();
        assert!(bp.has_attr_list);
        assert_eq!(bp.size, None);
        assert_eq!(ep.base_ref, 77);
        assert!(ep.names.is_empty());
        bp.merge(ep);
        assert_eq!(bp.size, Some(10_000));
        assert_eq!(bp.alloc, 3 * CL);
    }

    #[test]
    fn later_extent_does_not_override_size() {
        let mut rec = Rb::new(true, false).base(9).data_nonres(8, 0, 4).seal();
        let p = parse_record(&mut rec, 600, CL).unwrap();
        assert_eq!(p.size, None);
        assert_eq!(p.alloc, 4 * CL);
    }

    #[test]
    fn name_selection_prefers_win32_over_dos() {
        let mut rec = Rb::new(true, false)
            .name(5, "REALNA~1.TXT", 2)
            .name(5, "Real Name.txt", 1)
            .seal();
        let p = parse_record(&mut rec, 1, CL).unwrap();
        let sel = select_names(&p.names);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].name, "Real Name.txt");
    }

    #[test]
    fn owned_selection_matches_borrowed() {
        let mut rec = Rb::new(true, false)
            .name(5, "a.txt", 1)
            .name(9, "b.txt", 3)
            .name(9, "B~1.TXT", 2)
            .name(5, "a.txt", 1)
            .name(7, "ONLY~1.TXT", 2)
            .seal();
        let p = parse_record(&mut rec, 1, CL).unwrap();
        let a: Vec<NameAttr> = select_names(&p.names).into_iter().cloned().collect();
        assert_eq!(select_names_owned(p.names), a);
    }

    #[test]
    fn owned_selection_two_names_exhaustive() {
        for (pa, pb) in [(5u64, 5u64), (5, 9), (5 | (1 << 48), 5 | (2 << 48))] {
            for (na, nb) in [("x", "x"), ("x", "y")] {
                for (sa, sb) in (0..4u8).flat_map(|a| (0..4u8).map(move |b| (a, b))) {
                    let v = vec![
                        NameAttr { parent_ref: pa, namespace: sa, name: na.into() },
                        NameAttr { parent_ref: pb, namespace: sb, name: nb.into() },
                    ];
                    let want: Vec<NameAttr> = select_names(&v).into_iter().cloned().collect();
                    assert_eq!(select_names_owned(v.clone()), want, "{v:?}");
                }
            }
        }
    }

    #[test]
    fn utf16_lossy_matches_std() {
        let units: [u16; 6] = [b'a' as u16, 0xD83D, 0xDE00, b'z' as u16, 0xD800, 0xE9];
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(utf16le_to_string(&bytes), String::from_utf16_lossy(&units));
        assert_eq!(utf16le_to_string(b"h\x00i\x00"), "hi");
    }

    #[test]
    fn dos_only_is_fallback() {
        let mut rec = Rb::new(true, false).name(5, "ONLY~1.TXT", 2).seal();
        let p = parse_record(&mut rec, 1, CL).unwrap();
        let sel = select_names(&p.names);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].name, "ONLY~1.TXT");
    }

    #[test]
    fn hard_links_emit_one_per_parent_and_name() {
        let mut rec = Rb::new(true, false)
            .name(5, "a.txt", 1)
            .name(9, "b.txt", 3)
            .name(9, "B~1.TXT", 2)
            .name(5, "a.txt", 1) // exact dup
            .seal();
        let p = parse_record(&mut rec, 1, CL).unwrap();
        let sel: Vec<_> = select_names(&p.names).iter().map(|n| (n.parent_ref, n.name.clone())).collect();
        assert_eq!(sel, vec![(5, "a.txt".to_string()), (9, "b.txt".to_string())]);
    }

    #[test]
    fn malformed_attribute_stops_walk_without_panic() {
        let mut rec = Rb::new(true, false).name(5, "ok", 1).seal();
        // corrupt: attribute length overflows `used`
        rec[0x38 + 4..0x38 + 8].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        let p = parse_record(&mut rec, 1, CL).unwrap();
        assert!(p.names.is_empty());
    }

    #[test]
    fn unicode_name_roundtrip() {
        let mut rec = Rb::new(true, false).name(5, "日本語 ファイル.txt", 1).seal();
        let p = parse_record(&mut rec, 1, CL).unwrap();
        assert_eq!(p.names[0].name, "日本語 ファイル.txt");
    }

    #[test]
    fn data_runs_roundtrip_and_negative() {
        let r = [0x11u8, 8, 100, 0x01, 2, 0x00];
        assert_eq!(decode_data_runs(&r, 0, r.len()), vec![(8, Some(100)), (2, None)]);
        let r = [0x11u8, 4, 50, 0x11, 4, 0xEC];
        assert_eq!(decode_data_runs(&r, 0, r.len()), vec![(4, Some(50)), (4, Some(30))]);
        // truncated run header must not panic
        let r = [0x21u8, 4];
        assert!(decode_data_runs(&r, 0, r.len()).is_empty());
    }
}
