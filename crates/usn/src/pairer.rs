//! Rename pairing: the journal reports a rename as two records (old name,
//! then new name) for the same FRN. Park the old one until the new arrives.

use std::collections::{HashMap, VecDeque};

use crate::parse::{classify, Op, UsnRecord};

/// A normalized index mutation. Consumers must apply these idempotently
/// (find-or-insert by FRN): the journal can repeat Create/Modify for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// File or directory appeared (also: an unpaired RenameNew, i.e. the
    /// old name was never seen; treat as upsert/move by FRN).
    Create(UsnRecord),
    Delete(UsnRecord),
    /// Data/size/basic-info change; the index may want to refresh metadata.
    Modify(UsnRecord),
    Rename {
        frn: u64,
        old_parent: u64,
        old_name: String,
        new_parent: u64,
        new_name: String,
    },
}

pub struct RenamePairer {
    stale_after: u64,
    seen: u64,
    parked: HashMap<u64, (u64, UsnRecord)>,
    order: VecDeque<(u64, u64)>, // (seq, frn) in park order
}

impl RenamePairer {
    /// `stale_after`: an unmatched RenameOld is flushed as a Delete once more
    /// than this many records have been pushed after it.
    pub fn new(stale_after: u64) -> Self {
        Self { stale_after, seen: 0, parked: HashMap::new(), order: VecDeque::new() }
    }

    /// Number of currently parked RenameOld records.
    pub fn pending(&self) -> usize {
        self.parked.len()
    }

    /// Feed one record; returns the changes it completes (possibly including
    /// stale flushes). `Op::Other` records produce nothing.
    pub fn push(&mut self, rec: UsnRecord) -> Vec<Change> {
        self.seen += 1;
        let seq = self.seen;
        let mut out = Vec::new();
        match classify(rec.reason) {
            Op::RenameOld => {
                // A second old-name for the same FRN means the earlier new-name
                // was never seen; the earlier record is stale now.
                if let Some((_, prev)) = self.parked.insert(rec.frn, (seq, rec.clone())) {
                    out.push(Change::Delete(prev));
                }
                self.order.push_back((seq, rec.frn));
            }
            Op::RenameNew => match self.parked.remove(&rec.frn) {
                Some((_, old)) => out.push(Change::Rename {
                    frn: rec.frn,
                    old_parent: old.parent_frn,
                    old_name: old.name,
                    new_parent: rec.parent_frn,
                    new_name: rec.name,
                }),
                None => out.push(Change::Create(rec)),
            },
            Op::Delete => {
                self.parked.remove(&rec.frn);
                out.push(Change::Delete(rec));
            }
            Op::Create => out.push(Change::Create(rec)),
            Op::Modify => out.push(Change::Modify(rec)),
            Op::Other => {}
        }
        out.extend(self.flush_stale());
        out
    }

    fn flush_stale(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        while let Some(&(seq, frn)) = self.order.front() {
            if self.seen - seq <= self.stale_after {
                break;
            }
            self.order.pop_front();
            // Only flush if this queue entry still describes the parked record.
            if let Some((pseq, _)) = self.parked.get(&frn) {
                if *pseq == seq {
                    if let Some((_, rec)) = self.parked.remove(&frn) {
                        out.push(Change::Delete(rec));
                    }
                }
            }
        }
        out
    }

    /// Flush everything still parked as Deletes (e.g. on shutdown/reset).
    pub fn drain(&mut self) -> Vec<Change> {
        self.order.clear();
        let mut v: Vec<(u64, UsnRecord)> = self.parked.drain().map(|(_, v)| v).collect();
        v.sort_by_key(|(s, _)| *s);
        v.into_iter().map(|(_, r)| Change::Delete(r)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{
        USN_REASON_CLOSE, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
        USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
    };

    fn rec(frn: u64, parent: u64, reason: u32, name: &str) -> UsnRecord {
        UsnRecord { frn, parent_frn: parent, usn: 0, reason, attrs: 0, name: name.into() }
    }

    #[test]
    fn pairs_old_and_new() {
        let mut p = RenamePairer::new(100);
        assert!(p.push(rec(1, 5, USN_REASON_RENAME_OLD_NAME, "a")).is_empty());
        assert_eq!(p.pending(), 1);
        let out = p.push(rec(1, 9, USN_REASON_RENAME_NEW_NAME | USN_REASON_CLOSE, "b"));
        assert_eq!(
            out,
            vec![Change::Rename { frn: 1, old_parent: 5, old_name: "a".into(), new_parent: 9, new_name: "b".into() }]
        );
        assert_eq!(p.pending(), 0);
    }

    #[test]
    fn interleaved_pairs_by_frn() {
        let mut p = RenamePairer::new(100);
        p.push(rec(1, 5, USN_REASON_RENAME_OLD_NAME, "a"));
        p.push(rec(2, 5, USN_REASON_RENAME_OLD_NAME, "x"));
        let o2 = p.push(rec(2, 5, USN_REASON_RENAME_NEW_NAME, "y"));
        let o1 = p.push(rec(1, 5, USN_REASON_RENAME_NEW_NAME, "b"));
        assert!(matches!(&o2[0], Change::Rename { frn: 2, new_name, .. } if new_name == "y"));
        assert!(matches!(&o1[0], Change::Rename { frn: 1, old_name, .. } if old_name == "a"));
    }

    #[test]
    fn unpaired_new_becomes_create() {
        let mut p = RenamePairer::new(10);
        let out = p.push(rec(3, 5, USN_REASON_RENAME_NEW_NAME, "z"));
        assert!(matches!(&out[0], Change::Create(r) if r.name == "z"));
    }

    #[test]
    fn stale_old_flushes_as_delete() {
        let mut p = RenamePairer::new(2);
        p.push(rec(1, 5, USN_REASON_RENAME_OLD_NAME, "gone"));
        assert!(p.push(rec(10, 5, USN_REASON_FILE_CREATE, "c1")).len() == 1);
        assert!(p.push(rec(11, 5, USN_REASON_FILE_CREATE, "c2")).len() == 1);
        let out = p.push(rec(12, 5, USN_REASON_FILE_CREATE, "c3"));
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[1], Change::Delete(r) if r.frn == 1 && r.name == "gone"));
        assert_eq!(p.pending(), 0);
        // a late new-name after the flush degrades to Create
        let late = p.push(rec(1, 5, USN_REASON_RENAME_NEW_NAME, "late"));
        assert!(matches!(&late[0], Change::Create(_)));
    }

    #[test]
    fn delete_cancels_parked_old() {
        let mut p = RenamePairer::new(100);
        p.push(rec(1, 5, USN_REASON_RENAME_OLD_NAME, "a"));
        let out = p.push(rec(1, 5, USN_REASON_FILE_DELETE, "b"));
        assert!(matches!(&out[0], Change::Delete(_)));
        assert_eq!(p.pending(), 0);
    }

    #[test]
    fn repeated_old_flushes_previous() {
        let mut p = RenamePairer::new(100);
        p.push(rec(1, 5, USN_REASON_RENAME_OLD_NAME, "a"));
        let out = p.push(rec(1, 6, USN_REASON_RENAME_OLD_NAME, "b"));
        assert!(matches!(&out[0], Change::Delete(r) if r.name == "a"));
        let out = p.push(rec(1, 7, USN_REASON_RENAME_NEW_NAME, "c"));
        assert!(matches!(&out[0], Change::Rename { old_name, .. } if old_name == "b"));
    }

    #[test]
    fn other_ops_are_ignored_and_drain_works() {
        let mut p = RenamePairer::new(100);
        assert!(p.push(rec(1, 5, USN_REASON_CLOSE, "n")).is_empty());
        p.push(rec(2, 5, USN_REASON_RENAME_OLD_NAME, "o"));
        let d = p.drain();
        assert_eq!(d.len(), 1);
        assert_eq!(p.pending(), 0);
    }
}
