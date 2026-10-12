//! Pure index mutation logic (no Win32, no threads): applying USN [`Change`]s and
//! swapping one volume's records for a fresh scan. Unit-tested without admin.

use std::time::{SystemTime, UNIX_EPOCH};

use winseek_index::{Index, IndexBuilder};
use winseek_ipc::ChangeKind;
use winseek_usn::Change;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// What a [`Change`] did to the index (for the recent ring and subscribers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: ChangeKind,
    /// New path (for `Deleted`: the path the record had just before removal).
    pub path: String,
    pub old_path: Option<String>,
    pub is_dir: bool,
    /// Record index; `None` for `Deleted`. Only valid until the next mutation.
    pub idx: Option<u32>,
}

pub fn now_filetime() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    filetime_from_unix_ms(d.as_millis() as i64)
}

pub fn filetime_from_unix_ms(ms: i64) -> i64 {
    ms * 10_000 + FILETIME_UNIX_EPOCH
}

/// FILETIME -> unix milliseconds; `None` for 0/pre-1970 values (unknown).
pub fn filetime_to_unix_ms(ft: i64) -> Option<i64> {
    (ft > FILETIME_UNIX_EPOCH).then(|| (ft - FILETIME_UNIX_EPOCH) / 10_000)
}

/// Apply one change idempotently (the journal repeats Create/Modify, and catch-up replays
/// records the snapshot may already contain). Unknown parents become lazy stub folders.
/// USN records carry no size/time: new records get size 0 and mtime = now (the writer
/// refines both with a stat), `Modify` bumps mtime to now.
///
/// Limitations: a `Rename` of a *directory the index has never seen* is applied as a file
/// create (the rename record has no attributes); directory `Modify` records are ignored.
pub fn apply_change(ix: &mut Index, vol: u8, change: &Change) -> Option<Event> {
    let now = now_filetime();
    match change {
        Change::Create(r) => {
            let is_dir = r.attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            let known = if is_dir { ix.folder_idx(vol, r.frn) } else { ix.find_file(vol, r.parent_frn, &r.name) };
            let (size, mtime) = match known {
                Some(i) if !ix.is_stub(i) => (ix.size(i).unwrap_or(0), ix.mtime(i).filter(|&m| m != 0).unwrap_or(now)),
                _ => (0, now),
            };
            let i = ix.insert(vol, r.frn, r.parent_frn, &r.name, is_dir, size, mtime);
            Some(Event { kind: ChangeKind::Created, path: ix.full_path(i), old_path: None, is_dir, idx: Some(i) })
        }
        Change::Delete(r) => {
            let is_dir = r.attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            let i = if is_dir { ix.folder_idx(vol, r.frn) } else { ix.find_file(vol, r.parent_frn, &r.name) }?;
            let stub = ix.is_stub(i);
            let ev = Event { kind: ChangeKind::Deleted, path: ix.full_path(i), old_path: None, is_dir: ix.is_dir(i), idx: None };
            ix.remove(i);
            (!stub).then_some(ev)
        }
        Change::Rename { frn, old_parent, old_name, new_parent, new_name } => {
            let known_dir = ix.folder_idx(vol, *frn);
            if known_dir.is_some_and(|i| ix.is_stub(i)) {
                // Children were seen before their folder: this rename names the stub.
                let i = ix.insert(vol, *frn, *new_parent, new_name, true, 0, now);
                return Some(Event { kind: ChangeKind::Created, path: ix.full_path(i), old_path: None, is_dir: true, idx: Some(i) });
            }
            match known_dir.or_else(|| ix.find_file(vol, *old_parent, old_name)) {
                Some(i) => {
                    let old_path = ix.full_path(i);
                    let np = ix.ensure_folder(vol, *new_parent);
                    if !ix.rename(i, np, new_name) {
                        return None;
                    }
                    Some(Event {
                        kind: ChangeKind::Renamed,
                        path: ix.full_path(i),
                        old_path: Some(old_path),
                        is_dir: ix.is_dir(i),
                        idx: Some(i),
                    })
                }
                None => {
                    let i = ix.insert(vol, *frn, *new_parent, new_name, false, 0, now);
                    Some(Event { kind: ChangeKind::Created, path: ix.full_path(i), old_path: None, is_dir: false, idx: Some(i) })
                }
            }
        }
        Change::Modify(r) => {
            if r.attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                return None;
            }
            let i = match ix.find_file(vol, r.parent_frn, &r.name) {
                Some(i) => i,
                None => ix.insert(vol, r.frn, r.parent_frn, &r.name, false, 0, now),
            };
            let size = ix.size(i).unwrap_or(0);
            ix.update_meta(i, size, now);
            Some(Event { kind: ChangeKind::Modified, path: ix.full_path(i), old_path: None, is_dir: false, idx: Some(i) })
        }
    }
}

/// Copy the live, non-root, non-stub records of the volumes selected by `keep` from `src`.
fn copy_records(b: &mut IndexBuilder, src: &Index, keep: impl Fn(u8) -> bool) {
    for v in src.volumes().iter().filter(|v| keep(v.letter)) {
        b.add_volume(v.letter, 5);
        if let Some(m) = b.volume_mut(v.letter) {
            m.serial = v.serial;
            m.journal_id = v.journal_id;
            m.next_usn = v.next_usn;
        }
    }
    for i in 0..src.slots() as u32 {
        if src.is_deleted(i) || src.is_stub(i) {
            continue;
        }
        let vol = src.vol(i);
        if !keep(vol) || src.root_of(vol) == Some(i) {
            continue;
        }
        let pf = src.frn(src.parent(i));
        b.push(vol, src.frn(i), pf, src.name(i), src.is_dir(i), src.size(i).unwrap_or(0), src.mtime(i).unwrap_or(0));
    }
}

/// The index API has no "drop one volume's records" (roots cannot be removed), so replacing
/// a volume means building a new index: every other volume of `old` plus all of `fresh`
/// (a finished scan index holding the replaced volume). Cost is linear in total records.
pub fn replace_volume(old: &Index, fresh: &Index) -> Index {
    let new_letters: Vec<u8> = fresh.volumes().iter().map(|v| v.letter).collect();
    let mut b = IndexBuilder::with_capacity(true, old.live_count() + fresh.live_count());
    copy_records(&mut b, old, |v| !new_letters.contains(&v));
    copy_records(&mut b, fresh, |_| true);
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_index::SearchOptions;
    use winseek_usn::UsnRecord;

    const C: u8 = b'C';
    const D: u8 = b'D';

    fn rec(frn: u64, parent: u64, name: &str, dir: bool) -> UsnRecord {
        UsnRecord { frn, parent_frn: parent, usn: 0, reason: 0, attrs: if dir { 0x10 } else { 0x20 }, name: name.into() }
    }
    fn paths(ix: &Index, q: &str) -> Vec<String> {
        let mut v: Vec<String> =
            ix.search_str(q, &SearchOptions::default()).items.iter().map(|&i| ix.full_path(i)).collect();
        v.sort();
        v
    }
    /// C:\ { A(10) { x.txt }, B(11) }
    fn base() -> Index {
        let mut b = IndexBuilder::new(true);
        b.add_volume(C, 5);
        b.push(C, 10, 5, "A", true, 0, 0);
        b.push(C, 11, 5, "B", true, 0, 0);
        b.push(C, 100, 10, "x.txt", false, 42, 0);
        b.finish()
    }

    #[test]
    fn create_file_and_idempotent_replay() {
        let mut ix = base();
        let ev = apply_change(&mut ix, C, &Change::Create(rec(101, 10, "new.txt", false))).unwrap();
        assert_eq!((ev.kind, ev.path.as_str(), ev.is_dir), (ChangeKind::Created, "C:\\A\\new.txt", false));
        apply_change(&mut ix, C, &Change::Create(rec(101, 10, "new.txt", false)));
        assert_eq!(paths(&ix, "new.txt"), vec!["C:\\A\\new.txt"]);
        // replaying Create of an existing file keeps its size
        apply_change(&mut ix, C, &Change::Create(rec(100, 10, "x.txt", false)));
        let i = ix.find_file(C, 10, "x.txt").unwrap();
        assert_eq!(ix.size(i), Some(42));
    }

    #[test]
    fn rename_across_folders_and_folder_move() {
        let mut ix = base();
        let ev = apply_change(
            &mut ix,
            C,
            &Change::Rename { frn: 100, old_parent: 10, old_name: "x.txt".into(), new_parent: 11, new_name: "y.txt".into() },
        )
        .unwrap();
        assert_eq!(ev.kind, ChangeKind::Renamed);
        assert_eq!(ev.old_path.as_deref(), Some("C:\\A\\x.txt"));
        assert_eq!(ev.path, "C:\\B\\y.txt");
        assert!(paths(&ix, "x.txt").is_empty());
        assert_eq!(paths(&ix, "y.txt"), vec!["C:\\B\\y.txt"]);
        // move folder A under B, renamed: children follow
        apply_change(&mut ix, C, &Change::Create(rec(102, 10, "in.txt", false)));
        let ev = apply_change(
            &mut ix,
            C,
            &Change::Rename { frn: 10, old_parent: 5, old_name: "A".into(), new_parent: 11, new_name: "A2".into() },
        )
        .unwrap();
        assert!(ev.is_dir);
        assert_eq!(ev.path, "C:\\B\\A2");
        assert_eq!(paths(&ix, "in.txt"), vec!["C:\\B\\A2\\in.txt"]);
    }

    #[test]
    fn delete_file_and_folder_recursively() {
        let mut ix = base();
        apply_change(&mut ix, C, &Change::Create(rec(12, 10, "sub", true)));
        apply_change(&mut ix, C, &Change::Create(rec(103, 12, "deep.txt", false)));
        let ev = apply_change(&mut ix, C, &Change::Delete(rec(100, 10, "x.txt", false))).unwrap();
        assert_eq!((ev.kind, ev.path.as_str()), (ChangeKind::Deleted, "C:\\A\\x.txt"));
        let ev = apply_change(&mut ix, C, &Change::Delete(rec(10, 5, "A", true))).unwrap();
        assert!(ev.is_dir);
        assert_eq!(ev.path, "C:\\A");
        assert!(paths(&ix, "deep.txt").is_empty());
        assert!(ix.folder_idx(C, 12).is_none());
        ix.refresh();
        assert_eq!(paths(&ix, ""), vec!["C:\\B"]);
        // deleting something unknown is a no-op
        assert!(apply_change(&mut ix, C, &Change::Delete(rec(999, 5, "ghost", false))).is_none());
    }

    #[test]
    fn create_under_unknown_parent_uses_stub_then_fills() {
        let mut ix = base();
        let ev = apply_change(&mut ix, C, &Change::Create(rec(200, 77, "orphan.txt", false))).unwrap();
        assert_eq!(ev.path, "C:\\<4d>\\orphan.txt");
        assert!(paths(&ix, "<4d>").is_empty(), "stubs are not searchable");
        // the real folder shows up later: stub is filled in place, child follows
        apply_change(&mut ix, C, &Change::Create(rec(77, 5, "Late", true)));
        assert_eq!(paths(&ix, "orphan"), vec!["C:\\Late\\orphan.txt"]);
        assert_eq!(paths(&ix, "late"), vec!["C:\\Late"]);
    }

    #[test]
    fn modify_updates_or_inserts() {
        let mut ix = base();
        let ev = apply_change(&mut ix, C, &Change::Modify(rec(100, 10, "x.txt", false))).unwrap();
        assert_eq!(ev.kind, ChangeKind::Modified);
        let i = ev.idx.unwrap();
        assert_eq!(ix.size(i), Some(42));
        assert!(ix.mtime(i).unwrap() > FILETIME_UNIX_EPOCH);
        assert!(apply_change(&mut ix, C, &Change::Modify(rec(10, 5, "A", true))).is_none());
        apply_change(&mut ix, C, &Change::Modify(rec(300, 11, "unseen.bin", false))).unwrap();
        assert_eq!(paths(&ix, "unseen"), vec!["C:\\B\\unseen.bin"]);
    }

    #[test]
    fn rename_of_unknown_file_is_a_create() {
        let mut ix = base();
        let ev = apply_change(
            &mut ix,
            C,
            &Change::Rename { frn: 400, old_parent: 11, old_name: "nope".into(), new_parent: 11, new_name: "now.txt".into() },
        )
        .unwrap();
        assert_eq!((ev.kind, ev.path.as_str()), (ChangeKind::Created, "C:\\B\\now.txt"));
    }

    #[test]
    fn replace_volume_keeps_other_volumes() {
        let mut b = IndexBuilder::new(true);
        b.add_volume(C, 5);
        b.add_volume(D, 5);
        b.push(C, 10, 5, "old", true, 0, 0);
        b.push(C, 11, 10, "stale.txt", false, 1, 0);
        b.push(D, 20, 5, "keep", true, 0, 0);
        b.push(D, 21, 20, "d.txt", false, 5, 123);
        b.push(D, 22, 999, "orphan.txt", false, 0, 0); // stub parent
        let mut old = b.finish();
        old.volume_mut(D).unwrap().next_usn = 777;
        let mut f = IndexBuilder::new(true);
        f.add_volume(C, 5);
        f.push(C, 30, 5, "fresh", true, 0, 0);
        f.push(C, 31, 30, "new.txt", false, 9, 0);
        let mut fresh = f.finish();
        fresh.volume_mut(C).unwrap().journal_id = 5;

        let mut m = replace_volume(&old, &fresh);
        assert_eq!(
            paths(&m, ""),
            vec!["C:\\fresh", "C:\\fresh\\new.txt", "D:\\<3e7>\\orphan.txt", "D:\\keep", "D:\\keep\\d.txt"]
        );
        let i = m.find_file(D, 20, "d.txt").unwrap();
        assert_eq!(m.size(i), Some(5));
        assert_eq!(m.mtime(i), Some(123));
        assert_eq!(m.volumes().iter().find(|v| v.letter == D).unwrap().next_usn, 777);
        assert_eq!(m.volumes().iter().find(|v| v.letter == C).unwrap().journal_id, 5);
    }

    #[test]
    fn filetime_conversions() {
        assert_eq!(filetime_to_unix_ms(0), None);
        assert_eq!(filetime_to_unix_ms(filetime_from_unix_ms(1_700_000_000_123)), Some(1_700_000_000_123));
    }
}
