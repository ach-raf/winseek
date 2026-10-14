//! Pure (no Win32) `ReadDirectoryChangesW` buffer parsing and rename pairing.

pub const FILE_ACTION_ADDED: u32 = 1;
pub const FILE_ACTION_REMOVED: u32 = 2;
pub const FILE_ACTION_MODIFIED: u32 = 3;
pub const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
pub const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathEvent {
    Added(String),
    Removed(String),
    Modified(String),
    Renamed { old: String, new: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyError {
    /// "invalid rdc next entry offset": `offset` is the entry that carried the bad link.
    BadNextOffset { offset: usize },
    /// "invalid rdc file name length access".
    BadNameLength { offset: usize },
}

const HDR: usize = 12;

/// Parse a `FILE_NOTIFY_INFORMATION` chain into `(action, relative path)` pairs.
///
/// Guards: the next-entry offset must be 0 or >= 12, 4-aligned and leave room for a header
/// inside the buffer; the name length must be even and inside the buffer. A bad entry yields
/// an error (the caller rescans).
pub fn parse_notify(buf: &[u8]) -> Result<Vec<(u32, String)>, NotifyError> {
    let mut out = Vec::new();
    if buf.is_empty() {
        return Ok(out);
    }
    let mut off = 0usize;
    loop {
        if off + HDR > buf.len() {
            return Err(NotifyError::BadNextOffset { offset: off });
        }
        let rd = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let next = rd(off) as usize;
        let action = rd(off + 4);
        let name_len = rd(off + 8) as usize;
        if name_len % 2 != 0 || name_len > buf.len() - (off + HDR) {
            return Err(NotifyError::BadNameLength { offset: off });
        }
        let units: Vec<u16> = buf[off + HDR..off + HDR + name_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            return Ok(out);
        }
        if next < HDR || next % 4 != 0 || next > buf.len() - off || off + next + HDR > buf.len() {
            return Err(NotifyError::BadNextOffset { offset: off });
        }
        off += next;
    }
}

/// Turns raw actions into [`PathEvent`]s, parking a rename's old name (one slot).
#[derive(Debug, Default)]
pub struct NotifyPairer {
    parked: Option<String>,
}

impl NotifyPairer {
    pub fn new() -> Self {
        Self::default()
    }

    /// OLD parks (a second OLD replaces the first, which becomes `Removed`); NEW with a park
    /// -> `Renamed`, NEW without -> `Added`. Unknown actions are ignored.
    pub fn push(&mut self, action: u32, path: String) -> Option<PathEvent> {
        match action {
            FILE_ACTION_ADDED => Some(PathEvent::Added(path)),
            FILE_ACTION_REMOVED => Some(PathEvent::Removed(path)),
            FILE_ACTION_MODIFIED => Some(PathEvent::Modified(path)),
            FILE_ACTION_RENAMED_OLD_NAME => self.parked.replace(path).map(PathEvent::Removed),
            FILE_ACTION_RENAMED_NEW_NAME => Some(match self.parked.take() {
                Some(old) => PathEvent::Renamed { old, new: path },
                None => PathEvent::Added(path),
            }),
            _ => None,
        }
    }

    /// Push many actions, dropping a `Modified(p)` identical to the previous event in `out`.
    pub fn push_all(&mut self, raw: Vec<(u32, String)>, out: &mut Vec<PathEvent>) {
        for (action, path) in raw {
            if let Some(ev) = self.push(action, path) {
                if let PathEvent::Modified(p) = &ev {
                    if matches!(out.last(), Some(PathEvent::Modified(q)) if q == p) {
                        continue;
                    }
                }
                out.push(ev);
            }
        }
    }

    /// A parked old name that never got its NEW half is a removal.
    pub fn take_parked(&mut self) -> Option<PathEvent> {
        self.parked.take().map(PathEvent::Removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(next: u32, action: u32, name: &str) -> Vec<u8> {
        let u: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let mut v = Vec::new();
        v.extend(next.to_le_bytes());
        v.extend(action.to_le_bytes());
        v.extend((u.len() as u32).to_le_bytes());
        v.extend(u);
        v
    }

    /// Chain entries with correct (4-aligned) next offsets.
    fn chain(items: &[(u32, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, (a, n)) in items.iter().enumerate() {
            let mut e = entry(0, *a, n);
            while e.len() % 4 != 0 {
                e.push(0);
            }
            if i + 1 < items.len() {
                let l = e.len() as u32;
                e[0..4].copy_from_slice(&l.to_le_bytes());
            }
            out.extend(e);
        }
        out
    }

    #[test]
    fn parses_single_and_multiple() {
        assert_eq!(parse_notify(&[]).unwrap(), vec![]);
        let one = chain(&[(1, "a.txt")]);
        assert_eq!(parse_notify(&one).unwrap(), vec![(1, "a.txt".to_string())]);
        let many = chain(&[(1, "new.txt"), (3, "\u{fc}n\u{ef}-\u{65e5}\u{672c}"), (2, "a\\b.txt"), (4, "x")]);
        let p = parse_notify(&many).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p[1], (3, "\u{fc}n\u{ef}-\u{65e5}\u{672c}".to_string()));
        assert_eq!(p[2], (2, "a\\b.txt".to_string()));
        assert_eq!(p[3], (4, "x".to_string()));
    }

    #[test]
    fn guards() {
        let mut two = chain(&[(1, "ab"), (1, "cd")]);
        // next < 12
        two[0..4].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(parse_notify(&two), Err(NotifyError::BadNextOffset { offset: 0 }));
        // unaligned
        two[0..4].copy_from_slice(&17u32.to_le_bytes());
        assert_eq!(parse_notify(&two), Err(NotifyError::BadNextOffset { offset: 0 }));
        // past the end
        two[0..4].copy_from_slice(&4000u32.to_le_bytes());
        assert_eq!(parse_notify(&two), Err(NotifyError::BadNextOffset { offset: 0 }));
        // next entry would have no room for a header
        let n = two.len() as u32;
        two[0..4].copy_from_slice(&(n - 4).to_le_bytes());
        assert!(matches!(parse_notify(&two), Err(NotifyError::BadNextOffset { .. })));
        // odd name length
        let mut one = chain(&[(1, "ab")]);
        one[8..12].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(parse_notify(&one), Err(NotifyError::BadNameLength { offset: 0 }));
        // oversize name length
        one[8..12].copy_from_slice(&200u32.to_le_bytes());
        assert_eq!(parse_notify(&one), Err(NotifyError::BadNameLength { offset: 0 }));
        one[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_notify(&one), Err(NotifyError::BadNameLength { offset: 0 }));
        // shorter than a header
        assert!(parse_notify(&[1, 2, 3]).is_err());
    }

    #[test]
    fn fuzz_never_panics() {
        let base = chain(&[(1, "new.txt"), (3, "a\\b\u{fc}.txt"), (5, "z")]);
        for cut in 0..=base.len() {
            let _ = parse_notify(&base[..cut]);
        }
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut b = base.clone();
            let bit = (seed % (b.len() as u64 * 8)) as usize;
            b[bit / 8] ^= 1 << (bit % 8);
            let cut = (seed >> 20) as usize % (b.len() + 1);
            let _ = parse_notify(&b);
            let _ = parse_notify(&b[..cut]);
        }
    }

    #[test]
    fn pairer_rename_and_added() {
        let mut p = NotifyPairer::new();
        assert_eq!(p.push(4, "old".into()), None);
        assert_eq!(p.push(5, "new".into()), Some(PathEvent::Renamed { old: "old".into(), new: "new".into() }));
        assert_eq!(p.push(5, "alone".into()), Some(PathEvent::Added("alone".into())));
        assert_eq!(p.push(99, "x".into()), None);
        assert_eq!(p.take_parked(), None);
    }

    #[test]
    fn pairer_old_twice_and_take_parked() {
        let mut p = NotifyPairer::new();
        assert_eq!(p.push(4, "o1".into()), None);
        assert_eq!(p.push(4, "o2".into()), Some(PathEvent::Removed("o1".into())));
        assert_eq!(p.take_parked(), Some(PathEvent::Removed("o2".into())));
        assert_eq!(p.take_parked(), None);
    }

    #[test]
    fn push_all_coalesces_consecutive_modified() {
        let mut p = NotifyPairer::new();
        let mut out = Vec::new();
        let raw = vec![
            (3, "a".to_string()),
            (3, "a".to_string()),
            (3, "b".to_string()),
            (3, "a".to_string()),
            (1, "c".to_string()),
            (3, "a".to_string()),
            (4, "o".to_string()),
            (5, "n".to_string()),
        ];
        p.push_all(raw, &mut out);
        assert_eq!(
            out,
            vec![
                PathEvent::Modified("a".into()),
                PathEvent::Modified("b".into()),
                PathEvent::Modified("a".into()),
                PathEvent::Added("c".into()),
                PathEvent::Modified("a".into()),
                PathEvent::Renamed { old: "o".into(), new: "n".into() },
            ]
        );
    }
}
