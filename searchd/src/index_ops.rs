//! Pure index mutation logic (no Win32, no threads): applying USN [`Change`]s (NTFS, and
//! ReFS after FRN interning), path-based [`PathOp`]s (folder-watch volumes) and swapping one
//! volume's records for a fresh scan. Unit-tested without admin.

use std::time::{SystemTime, UNIX_EPOCH};

use winseek_index::{Index, IndexBuilder, VolumeMeta};
use winseek_ipc::ChangeKind;
use winseek_usn::{Change, UsnRecord};

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
    // NTFS FRNs are 64-bit (V2 records zero-extend); ReFS ones were interned to u64 ids
    // by `map_refs_change` before reaching here.
    match change {
        Change::Create(r) => {
            let (frn, parent) = (r.frn as u64, r.parent_frn as u64);
            let is_dir = r.attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            let known = if is_dir { ix.folder_idx(vol, frn) } else { ix.find_file(vol, parent, &r.name) };
            let (size, mtime) = match known {
                Some(i) if !ix.is_stub(i) => (ix.size(i).unwrap_or(0), ix.mtime(i).filter(|&m| m != 0).unwrap_or(now)),
                _ => (0, now),
            };
            let i = ix.insert(vol, frn, parent, &r.name, is_dir, size, mtime);
            Some(Event { kind: ChangeKind::Created, path: ix.full_path(i), old_path: None, is_dir, idx: Some(i) })
        }
        Change::Delete(r) => {
            let (frn, parent) = (r.frn as u64, r.parent_frn as u64);
            let is_dir = r.attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            let i = if is_dir { ix.folder_idx(vol, frn) } else { ix.find_file(vol, parent, &r.name) }?;
            let stub = ix.is_stub(i);
            let ev = Event { kind: ChangeKind::Deleted, path: ix.full_path(i), old_path: None, is_dir: ix.is_dir(i), idx: None };
            ix.remove(i);
            (!stub).then_some(ev)
        }
        Change::Rename { frn, old_parent, old_name, new_parent, new_name } => {
            let (frn, old_parent, new_parent) = (&(*frn as u64), &(*old_parent as u64), &(*new_parent as u64));
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
            let i = match ix.find_file(vol, r.parent_frn as u64, &r.name) {
                Some(i) => i,
                None => ix.insert(vol, r.frn as u64, r.parent_frn as u64, &r.name, false, 0, now),
            };
            let size = ix.size(i).unwrap_or(0);
            ix.update_meta(i, size, now);
            Some(Event { kind: ChangeKind::Modified, path: ix.full_path(i), old_path: None, is_dir: false, idx: Some(i) })
        }
    }
}

/// Translate a ReFS change (128-bit FRNs) into the u64 id space of the index, so the shared
/// [`apply_change`] can run on it. Everything keeps a twin folder tree keyed by 128-bit FRNs
/// (db+0x4a0); we intern instead: parents are always folders (find-or-assign), a directory's
/// own FRN is interned on create, and only looked up otherwise; files keep id 0 (they are
/// found by parent + name, like on NTFS).
pub fn map_refs_change(ix: &mut Index, vol: u8, change: &Change) -> Change {
    let rec = |ix: &mut Index, r: &UsnRecord, create: bool| {
        let parent = ix.intern128(vol, r.parent_frn) as u128;
        let frn = if r.attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            0
        } else if create {
            ix.intern128(vol, r.frn) as u128
        } else {
            ix.lookup128(vol, r.frn).unwrap_or(0) as u128
        };
        UsnRecord { frn, parent_frn: parent, ..r.clone() }
    };
    match change {
        Change::Create(r) => Change::Create(rec(ix, r, true)),
        Change::Delete(r) => Change::Delete(rec(ix, r, false)),
        Change::Modify(r) => Change::Modify(rec(ix, r, false)),
        Change::Rename { frn, old_parent, old_name, new_parent, new_name } => Change::Rename {
            frn: ix.lookup128(vol, *frn).unwrap_or(0) as u128,
            old_parent: ix.intern128(vol, *old_parent) as u128,
            old_name: old_name.clone(),
            new_parent: ix.intern128(vol, *new_parent) as u128,
            new_name: new_name.clone(),
        },
    }
}

/// A folder-watch change, already stat'ed by the monitor thread (no I/O under the index
/// lock). Paths are relative to the volume root (`dir\file.txt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOp {
    /// Added or modified: find-or-insert, refresh size/mtime.
    Upsert { rel: String, is_dir: bool, size: u64, mtime: i64 },
    Remove { rel: String },
    /// Rename/move; `is_dir`/`size`/`mtime` describe the new path (used if `old` is unknown).
    Rename { old: String, new: String, is_dir: bool, size: u64, mtime: i64 },
}

fn split_rel(rel: &str) -> (&str, &str) {
    let rel = rel.trim_matches('\\');
    match rel.rfind('\\') {
        Some(p) => (&rel[..p], &rel[p + 1..]),
        None => ("", rel),
    }
}

/// Resolve a folder path, creating missing folders (real names, fresh synthetic ids). A file
/// in the way of a folder component is replaced by the folder.
fn ensure_dir_path(ix: &mut Index, vol: u8, rel: &str) -> Option<u32> {
    let mut cur = ix.root_of(vol)?;
    for part in rel.split('\\').filter(|p| !p.is_empty()) {
        cur = match ix.find_child(cur, part) {
            Some(i) if ix.is_dir(i) => i,
            other => {
                if let Some(f) = other {
                    ix.remove(f);
                }
                let id = ix.alloc_id(vol);
                let pf = ix.frn(cur);
                ix.insert(vol, id, pf, part, true, 0, now_filetime())
            }
        };
    }
    Some(cur)
}

fn upsert(ix: &mut Index, vol: u8, rel: &str, is_dir: bool, size: u64, mtime: i64) -> Option<Event> {
    let (dir, name) = split_rel(rel);
    if name.is_empty() {
        return None;
    }
    let parent = ensure_dir_path(ix, vol, dir)?;
    if let Some(i) = ix.find_child(parent, name) {
        if ix.is_dir(i) == is_dir {
            if is_dir {
                return None; // folder metadata is not tracked; nothing changed for the index
            }
            ix.update_meta(i, size, mtime);
            return Some(Event { kind: ChangeKind::Modified, path: ix.full_path(i), old_path: None, is_dir, idx: Some(i) });
        }
        ix.remove(i); // a file became a folder or vice versa
    }
    let id = if is_dir { ix.alloc_id(vol) } else { 0 };
    let pf = ix.frn(parent);
    let i = ix.insert(vol, id, pf, name, is_dir, if is_dir { 0 } else { size }, mtime);
    Some(Event { kind: ChangeKind::Created, path: ix.full_path(i), old_path: None, is_dir, idx: Some(i) })
}

/// Apply one folder-watch op idempotently (replays after a rescan are expected).
pub fn apply_path_op(ix: &mut Index, vol: u8, op: &PathOp) -> Option<Event> {
    match op {
        PathOp::Upsert { rel, is_dir, size, mtime } => upsert(ix, vol, rel, *is_dir, *size, *mtime),
        PathOp::Remove { rel } => {
            let i = ix.find_path(vol, rel).filter(|&i| Some(i) != ix.root_of(vol))?;
            let ev = Event { kind: ChangeKind::Deleted, path: ix.full_path(i), old_path: None, is_dir: ix.is_dir(i), idx: None };
            ix.remove(i);
            Some(ev)
        }
        PathOp::Rename { old, new, is_dir, size, mtime } => {
            let Some(i) = ix.find_path(vol, old).filter(|&i| Some(i) != ix.root_of(vol)) else {
                return upsert(ix, vol, new, *is_dir, *size, *mtime);
            };
            let old_path = ix.full_path(i);
            let (dir, name) = split_rel(new);
            let np = ensure_dir_path(ix, vol, dir)?;
            if let Some(other) = ix.find_child(np, name).filter(|&o| o != i) {
                ix.remove(other); // rename over an existing entry
            }
            if !ix.rename(i, np, name) {
                return None;
            }
            Some(Event { kind: ChangeKind::Renamed, path: ix.full_path(i), old_path: Some(old_path), is_dir: ix.is_dir(i), idx: Some(i) })
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
            m.kind = v.kind;
            m.next_id = v.next_id;
        }
    }
    for (v, frn, id) in src.live_ids128() {
        if keep(v) {
            b.set_id128(v, frn, id);
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

/// Keep only the volumes `keep` accepts (a removed USB drive, a snapshot volume whose
/// filesystem changed). Returns `ix` untouched when nothing is dropped.
pub fn retain_volumes(ix: Index, keep: impl Fn(&VolumeMeta) -> bool) -> Index {
    let letters: Vec<u8> = ix.volumes().iter().filter(|v| keep(v)).map(|v| v.letter).collect();
    if letters.len() == ix.volumes().len() {
        return ix;
    }
    let mut b = IndexBuilder::with_capacity(ix.has_meta(), ix.live_count());
    copy_records(&mut b, &ix, |v| letters.contains(&v));
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_index::{SearchOptions, VolKind};
    use winseek_usn::UsnRecord;

    const C: u8 = b'C';
    const D: u8 = b'D';

    fn rec(frn: u128, parent: u128, name: &str, dir: bool) -> UsnRecord {
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

    const R: u8 = b'R';
    const F: u8 = b'F';
    const ROOT128: u128 = 0x0000_0000_0000_0700_0000_0000_0000_0600;

    /// A ReFS volume as the scan builds it: R:\Docs (FRN hi:lo 9:1) { a.txt }.
    fn refs() -> Index {
        let mut b = IndexBuilder::new(true);
        b.add_volume(R, 5);
        b.volume_mut(R).unwrap().kind = VolKind::Refs;
        b.set_id128(R, ROOT128, 5);
        let docs = b.intern128(R, (9 << 64) | 1);
        let root = b.intern128(R, ROOT128);
        b.push(R, docs, root, "Docs", true, 0, 0);
        b.push(R, 0, docs, "a.txt", false, 3, 0);
        b.finish()
    }

    fn refs_apply(ix: &mut Index, c: Change) -> Option<Event> {
        let m = map_refs_change(ix, R, &c);
        apply_change(ix, R, &m)
    }

    #[test]
    fn refs_changes_resolve_through_128_bit_frns() {
        let mut ix = refs();
        let docs = (9u128 << 64) | 1;
        // two FRNs that only differ in the high qword must stay distinct folders
        let sub_a = (1u128 << 64) | 0x42;
        let sub_b = (2u128 << 64) | 0x42;
        refs_apply(&mut ix, Change::Create(rec(sub_a, docs, "A", true))).unwrap();
        refs_apply(&mut ix, Change::Create(rec(sub_b, ROOT128, "B", true))).unwrap();
        let ev = refs_apply(&mut ix, Change::Create(rec(77, sub_a, "in-a.txt", false))).unwrap();
        assert_eq!(ev.path, r"R:\Docs\A\in-a.txt");
        let ev = refs_apply(&mut ix, Change::Create(rec(78, sub_b, "in-b.txt", false))).unwrap();
        assert_eq!(ev.path, r"R:\B\in-b.txt");
        // rename/move the folder A under B: children follow
        let ev = refs_apply(
            &mut ix,
            Change::Rename { frn: sub_a, old_parent: docs, old_name: "A".into(), new_parent: sub_b, new_name: "A2".into() },
        )
        .unwrap();
        assert_eq!((ev.kind, ev.path.as_str()), (ChangeKind::Renamed, r"R:\B\A2"));
        assert_eq!(paths(&ix, "in-a"), vec![r"R:\B\A2\in-a.txt"]);
        // file rename by parent + name
        let ev = refs_apply(
            &mut ix,
            Change::Rename { frn: 5000, old_parent: docs, old_name: "a.txt".into(), new_parent: docs, new_name: "b.txt".into() },
        )
        .unwrap();
        assert_eq!(ev.path, r"R:\Docs\b.txt");
        // folder delete by its 128-bit FRN; unknown FRNs are no-ops
        assert!(refs_apply(&mut ix, Change::Delete(rec((3 << 64) | 0x42, docs, "ghost", true))).is_none());
        let ev = refs_apply(&mut ix, Change::Delete(rec(sub_b, ROOT128, "B", true))).unwrap();
        assert!(ev.is_dir);
        assert!(paths(&ix, "in-").is_empty());
        // a child seen before its folder: stub by interned id, filled when the folder arrives
        let late = (5u128 << 64) | 7;
        let ev = refs_apply(&mut ix, Change::Create(rec(90, late, "early.txt", false))).unwrap();
        assert!(ev.path.contains('<'), "{}", ev.path);
        refs_apply(&mut ix, Change::Create(rec(late, docs, "Late", true)));
        assert_eq!(paths(&ix, "early"), vec![r"R:\Docs\Late\early.txt"]);
    }

    fn folder_vol() -> Index {
        let mut b = IndexBuilder::new(true);
        b.add_volume(F, 5);
        b.volume_mut(F).unwrap().kind = VolKind::Folder;
        let a = b.alloc_id(F);
        b.push(F, a, 5, "A", true, 0, 0);
        b.push(F, 0, a, "x.txt", false, 1, 0);
        b.finish()
    }
    fn up(rel: &str, is_dir: bool, size: u64) -> PathOp {
        PathOp::Upsert { rel: rel.into(), is_dir, size, mtime: 1000 }
    }

    #[test]
    fn path_ops_upsert_remove_rename() {
        let mut ix = folder_vol();
        // add in an existing folder, then a deep path whose folders are missing
        let ev = apply_path_op(&mut ix, F, &up(r"A\new.txt", false, 9)).unwrap();
        assert_eq!((ev.kind, ev.path.as_str()), (ChangeKind::Created, r"F:\A\new.txt"));
        apply_path_op(&mut ix, F, &up(r"B\C\deep.txt", false, 2)).unwrap();
        assert_eq!(paths(&ix, "deep"), vec![r"F:\B\C\deep.txt"]);
        assert_eq!(paths(&ix, "C"), vec![r"F:\B\C"]);
        // replay = modify, not a duplicate
        let ev = apply_path_op(&mut ix, F, &up(r"A\new.txt", false, 10)).unwrap();
        assert_eq!(ev.kind, ChangeKind::Modified);
        assert_eq!(ix.size(ev.idx.unwrap()), Some(10));
        assert_eq!(paths(&ix, "new.txt").len(), 1);
        assert!(apply_path_op(&mut ix, F, &up("A", true, 0)).is_none(), "existing folder: no event");
        // folder ids are fresh and unique
        let ids: Vec<u64> = ["A", "B", r"B\C"].iter().map(|p| { let i = ix.find_path(F, p).unwrap(); ix.frn(i) }).collect();
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids.iter().all(|&i| i >= winseek_index::FIRST_SYNTHETIC_ID));
        // move a folder: children follow
        let ev = apply_path_op(
            &mut ix,
            F,
            &PathOp::Rename { old: "B".into(), new: r"A\B2".into(), is_dir: true, size: 0, mtime: 0 },
        )
        .unwrap();
        assert_eq!((ev.kind, ev.path.as_str(), ev.old_path.as_deref()), (ChangeKind::Renamed, r"F:\A\B2", Some(r"F:\B")));
        assert_eq!(paths(&ix, "deep"), vec![r"F:\A\B2\C\deep.txt"]);
        // rename over an existing file replaces it
        apply_path_op(&mut ix, F, &PathOp::Rename { old: r"A\new.txt".into(), new: r"A\x.txt".into(), is_dir: false, size: 0, mtime: 0 }).unwrap();
        assert_eq!(paths(&ix, "x.txt"), vec![r"F:\A\x.txt"]);
        assert!(paths(&ix, "new.txt").is_empty());
        // rename of an unknown path = add
        let ev = apply_path_op(&mut ix, F, &PathOp::Rename { old: "nope".into(), new: "yes.txt".into(), is_dir: false, size: 4, mtime: 0 }).unwrap();
        assert_eq!((ev.kind, ev.path.as_str()), (ChangeKind::Created, r"F:\yes.txt"));
        // recursive remove; removing the unknown or the root is a no-op
        let ev = apply_path_op(&mut ix, F, &PathOp::Remove { rel: "A".into() }).unwrap();
        assert!(ev.is_dir);
        assert!(paths(&ix, "deep").is_empty());
        assert!(apply_path_op(&mut ix, F, &PathOp::Remove { rel: "A".into() }).is_none());
        assert!(apply_path_op(&mut ix, F, &PathOp::Remove { rel: String::new() }).is_none());
        // a file replaced by a folder of the same name
        apply_path_op(&mut ix, F, &up("yes.txt", true, 0)).unwrap();
        let i = ix.find_path(F, "yes.txt").unwrap();
        assert!(ix.is_dir(i));
        ix.refresh();
        assert_eq!(paths(&ix, ""), vec![r"F:\yes.txt"]);
    }

    #[test]
    fn retain_and_replace_keep_kinds_and_interned_ids() {
        let mut b = IndexBuilder::new(true);
        b.add_volume(C, 5);
        b.push(C, 10, 5, "c-dir", true, 0, 0);
        let mut ix = b.finish();
        // merge in a ReFS and a folder volume via replace_volume
        ix = replace_volume(&ix, &refs());
        ix = replace_volume(&ix, &folder_vol());
        let kinds: Vec<_> = ix.volumes().iter().map(|v| (v.letter, v.kind)).collect();
        assert!(kinds.contains(&(R, VolKind::Refs)) && kinds.contains(&(F, VolKind::Folder)) && kinds.contains(&(C, VolKind::Ntfs)));
        assert_eq!(ix.lookup128(R, ROOT128), Some(5));
        let docs_id = ix.lookup128(R, (9 << 64) | 1).unwrap();
        assert_eq!(ix.folder_idx(R, docs_id).map(|i| ix.full_path(i)), Some(r"R:\Docs".into()));
        let next = ix.volumes().iter().find(|v| v.letter == F).unwrap().next_id;
        assert_eq!(ix.alloc_id(F), next, "next_id survives the copy");
        // drop the folder volume (USB unplugged)
        let ix = retain_volumes(ix, |v| v.letter != F);
        assert!(ix.root_of(F).is_none());
        assert_eq!(paths(&ix, "x.txt"), Vec::<String>::new());
        assert_eq!(paths(&ix, "a.txt"), vec![r"R:\Docs\a.txt"]);
        assert_eq!(ix.lookup128(R, (9 << 64) | 1), Some(docs_id));
        // nothing to drop: same index back
        let n = ix.live_count();
        assert_eq!(retain_volumes(ix, |_| true).live_count(), n);
    }

    #[test]
    fn filetime_conversions() {
        assert_eq!(filetime_to_unix_ms(0), None);
        assert_eq!(filetime_to_unix_ms(filetime_from_unix_ms(1_700_000_000_123)), Some(1_700_000_000_123));
    }
}
