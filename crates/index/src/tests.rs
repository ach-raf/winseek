use crate::*;

const C: u8 = b'C';

/// C:\ (frn 5)
///   Users (10)
///     Alice (11)
///       Été.txt, report.PDF, photo.jpg
///     Bob (12)
///       report.txt, notes.md
///   Windows (20)
///     System32 (21)
///       kernel32.dll, report.txt
///   مرحبا.txt, 日本語.doc
fn sample() -> Index {
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    let mut n = 100u64;
    let mut push = |b: &mut IndexBuilder, parent: u64, name: &str, dir: bool, frn: Option<u64>| {
        n += 1;
        b.push(C, frn.unwrap_or(n), parent, name, dir, 7, 9);
    };
    // files before their parent folder to exercise forward references
    push(&mut b, 11, "Été.txt", false, None);
    push(&mut b, 11, "report.PDF", false, None);
    push(&mut b, 11, "photo.jpg", false, None);
    push(&mut b, 5, "Users", true, Some(10));
    push(&mut b, 10, "Alice", true, Some(11));
    push(&mut b, 10, "Bob", true, Some(12));
    push(&mut b, 12, "report.txt", false, None);
    push(&mut b, 12, "notes.md", false, None);
    push(&mut b, 5, "Windows", true, Some(20));
    push(&mut b, 20, "System32", true, Some(21));
    push(&mut b, 21, "kernel32.dll", false, None);
    push(&mut b, 21, "report.txt", false, None);
    push(&mut b, 5, "مرحبا.txt", false, None);
    push(&mut b, 5, "日本語.doc", false, None);
    b.finish()
}

fn names(ix: &Index, q: &str) -> Vec<String> {
    ix.search_str(q, &SearchOptions::default()).items.iter().map(|&i| ix.full_path(i)).collect()
}

#[test]
fn builder_resolves_parents_and_paths() {
    let ix = sample();
    let alice = ix.folder_idx(C, 11).unwrap();
    assert_eq!(ix.full_path(alice), "C:\\Users\\Alice");
    assert_eq!(ix.full_path(ix.root_of(C).unwrap()), "C:\\");
    assert!(names(&ix, "kernel32").contains(&"C:\\Windows\\System32\\kernel32.dll".to_string()));
    assert_eq!(names(&ix, "").len(), 14);
    assert_eq!(ix.size(alice), Some(7));
}

#[test]
fn lazy_stub_for_unknown_parent() {
    let mut b = IndexBuilder::new(false);
    b.add_volume(C, 5);
    b.push(C, 50, 999, "orphan.txt", false, 0, 0);
    let mut ix = b.finish();
    let o = names(&ix, "orphan")[0].clone();
    assert!(o.starts_with("C:\\<3e7>\\"), "{o}");
    // real folder arrives later via USN: stub filled in place, child path fixed
    ix.insert(C, 999, 5, "Real", true, 0, 0);
    assert_eq!(names(&ix, "orphan"), vec!["C:\\Real\\orphan.txt".to_string()]);
    assert_eq!(names(&ix, "real").len(), 1);
    // insert under unknown parent
    let f = ix.insert(C, 60, 777, "x.bin", false, 0, 0);
    assert!(ix.full_path(f).starts_with("C:\\<309>"));
    assert!(ix.is_stub(ix.folder_idx(C, 777).unwrap()));
}

#[test]
fn frn_sequence_bits_are_masked() {
    let mut ix = sample();
    let seq = 7u64 << 48;
    let f = ix.insert(C, 500 | seq, 12 | seq, "seq.txt", false, 0, 0);
    assert_eq!(ix.full_path(f), "C:\\Users\\Bob\\seq.txt");
    assert!(ix.find_file(C, 12, "seq.txt").is_some());
}

#[test]
fn insert_remove_rename() {
    let mut ix = sample();
    let n0 = names(&ix, "").len();
    let f = ix.insert(C, 900, 12, "new_file.rs", false, 1, 2);
    assert_eq!(ix.full_path(f), "C:\\Users\\Bob\\new_file.rs");
    // idempotent
    assert_eq!(ix.insert(C, 901, 12, "new_file.rs", false, 5, 6), f);
    assert_eq!(ix.size(f), Some(5));
    assert_eq!(names(&ix, "new_file").len(), 1); // visible on a dirty index
    ix.refresh();
    assert_eq!(names(&ix, "").len(), n0 + 1);

    // rename + move file Bob\new_file.rs -> Alice\Renamed.RS
    let alice = ix.folder_idx(C, 11).unwrap();
    assert!(ix.rename(f, alice, "Renamed.RS"));
    assert_eq!(names(&ix, "new_file").len(), 0);
    assert_eq!(names(&ix, "renamed"), vec!["C:\\Users\\Alice\\Renamed.RS".to_string()]);
    ix.refresh();
    assert_eq!(names(&ix, "renamed").len(), 1);
    assert_eq!(ix.find_file(C, 11, "Renamed.RS"), Some(f));
    assert_eq!(ix.find_file(C, 12, "new_file.rs"), None);

    // remove file by (parent, name)
    assert_eq!(ix.remove_file(C, 11, "Renamed.RS"), 1);
    assert_eq!(names(&ix, "renamed").len(), 0);
    ix.refresh();
    assert_eq!(names(&ix, "").len(), n0);

    // recursive folder delete: Users + Alice + 3 files + Bob + 2 files = 8 records
    assert_eq!(ix.remove_folder(C, 10), 8);
    ix.refresh();
    assert_eq!(names(&ix, "report"), vec!["C:\\Windows\\System32\\report.txt".to_string()]);
    assert!(ix.folder_idx(C, 11).is_none());
    // slots are recycled
    let slots = ix.slots();
    for k in 0..8 {
        ix.insert(C, 2000 + k, 20, &format!("r{k}"), false, 0, 0);
    }
    assert_eq!(ix.slots(), slots);
    ix.refresh();
    assert!(names(&ix, "r").len() >= 7);
}

#[test]
fn folder_rename_updates_paths_and_rejects_cycles() {
    let mut ix = sample();
    let users = ix.folder_idx(C, 10).unwrap();
    let win = ix.folder_idx(C, 20).unwrap();
    assert!(ix.rename(users, win, "UsersMoved"));
    assert_eq!(names(&ix, "photo"), vec!["C:\\Windows\\UsersMoved\\Alice\\photo.jpg".to_string()]);
    let alice = ix.folder_idx(C, 11).unwrap();
    assert!(!ix.rename(users, alice, "loop")); // into own descendant
    assert!(!ix.rename(ix.root_of(C).unwrap(), win, "x"));
    ix.refresh();
    assert_eq!(names(&ix, "UsersMoved").len(), 1);
}

#[test]
fn query_features() {
    let ix = sample();
    // AND
    assert_eq!(names(&ix, "report txt").len(), 2);
    // phrase
    assert_eq!(names(&ix, "\"report.txt\"").len(), 2);
    assert_eq!(names(&ix, "\"port.t\"").len(), 2);
    assert_eq!(names(&ix, "\"report txt\"").len(), 0);
    // negation
    assert_eq!(names(&ix, "report !txt").len(), 1);
    assert_eq!(names(&ix, "report !\"report.txt\""), vec!["C:\\Users\\Alice\\report.PDF".to_string()]);
    // wildcards: whole-name
    assert_eq!(names(&ix, "*.txt").len(), 4); // Été, report x2, مرحبا
    assert_eq!(names(&ix, "report.t?t").len(), 2);
    assert_eq!(names(&ix, "report").len(), 3);
    assert_eq!(names(&ix, "port.t?").len(), 0); // whole name when wildcard present
    assert_eq!(names(&ix, "report.*").len(), 3);
    // ext
    assert_eq!(names(&ix, "ext:pdf").len(), 1);
    assert_eq!(names(&ix, "ext:JPG;pdf").len(), 2);
    assert_eq!(names(&ix, "ext:.md report").len(), 0);
    assert_eq!(names(&ix, "ext:txt report").len(), 2);
    // file / folder
    let all = names(&ix, "").len();
    let folders = names(&ix, "folder:").len();
    assert_eq!(folders, 5); // Users Alice Bob Windows System32
    assert_eq!(names(&ix, "file:").len(), all - folders);
    assert_eq!(names(&ix, "folder:alice"), vec!["C:\\Users\\Alice".to_string()]);
    assert_eq!(names(&ix, "!folder: bob").len(), 0);
    assert_eq!(names(&ix, "file:report").len(), 3);
    assert_eq!(names(&ix, "folder:\"system32\"").len(), 1);
    // path terms
    assert_eq!(names(&ix, "system32\\report").len(), 1);
    assert_eq!(names(&ix, "users\\bob\\").len(), 2);
    assert_eq!(names(&ix, r"C:\Windows").len(), 4); // Windows, System32, 2 files
    assert_eq!(names(&ix, "users/alice").len(), 4);
    assert_eq!(names(&ix, "*\\bob\\*.txt").len(), 1);
    assert_eq!(names(&ix, "!users\\ *.txt").len(), 2);
}

#[test]
fn case_insensitive_unicode() {
    let ix = sample();
    assert_eq!(names(&ix, "ÉTÉ"), vec!["C:\\Users\\Alice\\Été.txt".to_string()]);
    assert_eq!(names(&ix, "été").len(), 1);
    assert_eq!(names(&ix, "ÉTÉ.TXT").len(), 1);
    assert_eq!(names(&ix, "*té.t?t").len(), 1);
    assert_eq!(names(&ix, "ext:TXT été").len(), 1);
    assert_eq!(names(&ix, "مرحبا").len(), 1);
    assert_eq!(names(&ix, "日本").len(), 1);
    assert_eq!(names(&ix, "日本語.DOC").len(), 1);
    assert_eq!(names(&ix, "KERNEL32").len(), 1);
    assert_eq!(names(&ix, "users\\ALICE\\été").len(), 1);
}

#[test]
fn sort_order_offset_limit() {
    let mut b = IndexBuilder::new(false);
    b.add_volume(C, 5);
    b.push(C, 10, 5, "b", true, 0, 0);
    b.push(C, 11, 5, "a", true, 0, 0);
    for (i, n) in ["delta", "Alpha", "charlie", "Bravo", "echo", "ÉCLAIR", "zulu"].iter().enumerate() {
        b.push(C, 100 + i as u64, 5, n, false, 0, 0);
    }
    // equal names -> tie by path (a\x before b\x)
    b.push(C, 200, 10, "same", false, 0, 0);
    b.push(C, 201, 11, "same", false, 0, 0);
    let mut ix = b.finish();
    let o = |ix: &Index, q: &str, off, lim, desc| {
        let r = ix.search_str(q, &SearchOptions { offset: off, limit: lim, descending: desc });
        (r.total, r.items.iter().map(|&i| ix.name(i).to_string()).collect::<Vec<_>>())
    };
    let (t, all) = o(&ix, "file:", 0, usize::MAX, false);
    assert_eq!(all, ["Alpha", "Bravo", "charlie", "delta", "echo", "same", "same", "zulu", "ÉCLAIR"]);
    assert_eq!(t, 9);
    let (t, page) = o(&ix, "file:", 2, 3, false);
    assert_eq!((t, page), (9, vec!["charlie".into(), "delta".into(), "echo".into()]));
    let (t, rev) = o(&ix, "file:", 0, usize::MAX, true);
    assert_eq!(t, 9);
    let mut exp = all.clone();
    exp.reverse();
    assert_eq!(rev, exp);
    let (_, page) = o(&ix, "file:", 1, 2, true);
    assert_eq!(page, vec!["zulu".to_string(), "same".to_string()]);
    assert_eq!(o(&ix, "file:", 100, 5, false), (9, vec![]));
    let sames = ix.search_str("same", &SearchOptions::default()).items;
    assert_eq!(ix.full_path(sames[0]), "C:\\a\\same");
    assert_eq!(ix.full_path(sames[1]), "C:\\b\\same");
    // incremental maintenance keeps order
    ix.insert(C, 300, 5, "bravo2", false, 0, 0);
    ix.insert(C, 301, 5, "aaa", false, 0, 0);
    let before = o(&ix, "file:", 0, usize::MAX, false).1; // dirty path
    ix.refresh();
    let after = o(&ix, "file:", 0, usize::MAX, false).1;
    assert_eq!(before, after);
    assert_eq!(after[0], "aaa");
    let mut sorted = after.clone();
    sorted.sort_by(|a, b| crate::fold::cmp_fold(a, b));
    assert_eq!(after, sorted);
}

#[test]
fn incremental_equals_full_rebuild() {
    let mut ix = sample();
    for k in 0..200u64 {
        let p = [5u64, 10, 11, 12, 20, 21][(k % 6) as usize];
        ix.insert(C, 5000 + k, p, &format!("file{}", (k * 7919) % 97), k % 5 == 0, 0, 0);
        if k % 9 == 0 {
            let f = ix.find_file(C, p, &format!("file{}", ((k.saturating_sub(3)) * 7919) % 97));
            if let Some(f) = f {
                ix.remove(f);
            }
        }
        if k % 13 == 0 {
            ix.refresh();
        }
    }
    ix.refresh();
    let inc: Vec<String> = ix.order.iter().map(|&i| ix.full_path(i)).collect();
    ix.rebuild_order();
    let full: Vec<String> = ix.order.iter().map(|&i| ix.full_path(i)).collect();
    assert_eq!(inc, full);
}

#[test]
fn snapshot_round_trip_and_corruption() {
    let mut ix = sample();
    ix.volume_mut(C).unwrap().serial = 0xAABB;
    ix.volume_mut(C).unwrap().journal_id = 77;
    ix.volume_mut(C).unwrap().next_usn = -5;
    ix.insert(C, 800, 12, "added.dat", false, 3, 4); // dirty at save time
    ix.remove_file(C, 12, "notes.md");
    let dir = std::env::temp_dir().join(format!("wsix_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("a.wsix");
    ix.save(&p).unwrap();
    assert!(!dir.join("a.wsix.tmp").exists());
    let mut ld = Index::load(&p).unwrap();
    let v = &ld.volumes()[0];
    assert_eq!((v.letter, v.serial, v.journal_id, v.next_usn), (C, 0xAABB, 77, -5));
    ix.refresh();
    let a: Vec<_> = ix.order.iter().map(|&i| (ix.full_path(i), ix.size(i), ix.frn(i))).collect();
    let b: Vec<_> = ld.order.iter().map(|&i| (ld.full_path(i), ld.size(i), ld.frn(i))).collect();
    assert_eq!(a, b);
    assert_eq!(names(&ld, "ÉTÉ"), names(&ix, "ÉTÉ"));
    // loaded index is fully mutable
    assert_eq!(ld.remove_file(C, 12, "added.dat"), 1);
    assert!(ld.find_file(C, 12, "report.txt").is_some());
    ld.insert(C, 801, 12, "again", false, 0, 0);
    ld.refresh();
    assert_eq!(names(&ld, "again").len(), 1);

    let bytes = ix.to_bytes();
    // truncations
    for cut in [0, 3, 8, 31, 32, bytes.len() / 2, bytes.len() - 1] {
        assert!(Index::from_bytes(&bytes[..cut]).is_err(), "cut {cut}");
    }
    // every single-byte flip is rejected (checksum) and never panics
    for k in (0..bytes.len()).step_by(7) {
        let mut m = bytes.clone();
        m[k] ^= 0x5a;
        assert!(Index::from_bytes(&m).is_err(), "flip {k}");
    }
    // structurally corrupt but with a valid checksum must also be rejected, not panic
    let mut m = bytes.clone();
    m.truncate(m.len() - 8);
    m[14] = 0xff; // absurd volume count
    let ck = super::snapshot::checksum_for_tests(&m);
    m.extend_from_slice(&ck.to_le_bytes());
    assert!(Index::from_bytes(&m).is_err());
    assert!(Index::load(&dir.join("missing.wsix")).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fuzz_garbage_never_panics() {
    let ix = sample();
    let good = ix.to_bytes();
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..300 {
        let mut m = good.clone();
        m.truncate(m.len() - 8);
        for _ in 0..3 {
            let k = (rnd() as usize) % m.len();
            m[k] = rnd() as u8;
        }
        let ck = super::snapshot::checksum_for_tests(&m);
        m.extend_from_slice(&ck.to_le_bytes());
        let _ = Index::from_bytes(&m); // Ok or Err, never a panic
    }
}

/// Brute-force recount: live, non-stub, non-root records per volume.
fn recount(ix: &Index, vol: u8) -> (u64, u64) {
    let (mut files, mut folders) = (0, 0);
    for i in 0..ix.slots() as u32 {
        if ix.is_deleted(i) || ix.is_stub(i) || ix.root_of(ix.vol(i)) == Some(i) || ix.vol(i) != vol {
            continue;
        }
        if ix.is_dir(i) {
            folders += 1;
        } else {
            files += 1;
        }
    }
    (files, folders)
}

#[test]
fn incremental_counts_match_recount() {
    let mut ix = sample();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    let mut dirs: Vec<u64> = vec![5];
    let mut next = 10_000u64;
    for step in 0..3000 {
        match rnd(7) {
            0..=2 => {
                let p = dirs[rnd(dirs.len() as u64) as usize];
                let is_dir = rnd(4) == 0;
                ix.insert(C, next, p, &format!("n{}", rnd(60)), is_dir, 0, 0);
                if is_dir {
                    dirs.push(next);
                }
                next += 1;
            }
            3 => {
                // orphan: unknown parent creates a stub, later possibly filled
                ix.insert(C, next, 900_000 + rnd(5), &format!("o{}", rnd(10)), false, 0, 0);
                next += 1;
            }
            4 => {
                // fill a stub (or no-op)
                ix.insert(C, 900_000 + rnd(5), 5, &format!("filled{}", rnd(5)), true, 0, 0);
            }
            5 => {
                let f = dirs[rnd(dirs.len() as u64) as usize];
                if f != 5 {
                    ix.remove_folder(C, f); // recursive
                    dirs.retain(|&d| ix.folder_idx(C, d).is_some());
                }
            }
            _ => {
                let p = dirs[rnd(dirs.len() as u64) as usize];
                ix.remove_file(C, p, &format!("n{}", rnd(60)));
                if let (Some(&a), Some(&b)) = (dirs.get(rnd(dirs.len() as u64) as usize), dirs.get(rnd(dirs.len() as u64) as usize)) {
                    if let (Some(ai), Some(bi)) = (ix.folder_idx(C, a), ix.folder_idx(C, b)) {
                        ix.rename(ai, bi, &format!("r{}", rnd(60)));
                    }
                }
            }
        }
        if step % 50 == 0 {
            ix.refresh();
        }
        if step % 97 == 0 {
            assert_eq!(ix.counts(C), recount(&ix, C), "step {step}");
        }
    }
    ix.refresh();
    assert_eq!(ix.counts(C), recount(&ix, C));
    let rt = Index::from_bytes(&ix.to_bytes()).unwrap();
    assert_eq!(rt.counts(C), ix.counts(C));
    assert_eq!(ix.counts(b'Z'), (0, 0));
}
