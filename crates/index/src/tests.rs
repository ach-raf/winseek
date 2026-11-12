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
    let o = |ix: &Index, q: &str, off, lim, order| {
        let r = ix.search_str(q, &SearchOptions { offset: off, limit: lim, order });
        (r.total, r.items.iter().map(|&i| ix.name(i).to_string()).collect::<Vec<_>>())
    };
    let (t, all) = o(&ix, "file:", 0, usize::MAX, Order::NameAsc);
    assert_eq!(all, ["Alpha", "Bravo", "charlie", "delta", "echo", "same", "same", "zulu", "ÉCLAIR"]);
    assert_eq!(t, 9);
    let (t, page) = o(&ix, "file:", 2, 3, Order::NameAsc);
    assert_eq!((t, page), (9, vec!["charlie".into(), "delta".into(), "echo".into()]));
    let (t, rev) = o(&ix, "file:", 0, usize::MAX, Order::NameDesc);
    assert_eq!(t, 9);
    let mut exp = all.clone();
    exp.reverse();
    assert_eq!(rev, exp);
    let (_, page) = o(&ix, "file:", 1, 2, Order::NameDesc);
    assert_eq!(page, vec!["zulu".to_string(), "same".to_string()]);
    assert_eq!(o(&ix, "file:", 100, 5, Order::NameAsc), (9, vec![]));
    let sames = ix.search_str("same", &SearchOptions::default()).items;
    assert_eq!(ix.full_path(sames[0]), "C:\\a\\same");
    assert_eq!(ix.full_path(sames[1]), "C:\\b\\same");
    // incremental maintenance keeps order
    ix.insert(C, 300, 5, "bravo2", false, 0, 0);
    ix.insert(C, 301, 5, "aaa", false, 0, 0);
    let before = o(&ix, "file:", 0, usize::MAX, Order::NameAsc).1; // dirty path
    ix.refresh();
    let after = o(&ix, "file:", 0, usize::MAX, Order::NameAsc).1;
    assert_eq!(before, after);
    assert_eq!(after[0], "aaa");
    let mut sorted = after.clone();
    sorted.sort_by(|a, b| crate::fold::cmp_fold(a, b));
    assert_eq!(after, sorted);
}

#[test]
fn meta_orders_sort_and_page() {
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    // (frn, name, size, mtime) — mtimes/size deliberately out of name order; two rows share an mtime.
    let rows = [
        (100u64, "old_small.txt", 10u64, 100i64),
        (101, "new_big.txt", 900, 500),
        (102, "mid.txt", 500, 300),
        (103, "same_a.txt", 50, 400),
        (104, "same_b.txt", 20, 400),
        (105, "old_big.txt", 800, 200),
    ];
    for (frn, name, size, mtime) in rows {
        b.push(C, frn, 5, name, false, size, mtime);
    }
    let mut ix = b.finish();
    let q = "file:";
    let get = |ix: &Index, off: usize, lim: usize, order: Order| -> Vec<(String, u64, i64)> {
        ix.search_str(q, &SearchOptions { offset: off, limit: lim, order })
            .items
            .iter()
            .map(|&i| (ix.name(i).to_string(), ix.size(i).unwrap(), ix.mtime(i).unwrap()))
            .collect()
    };

    // newest first; the equal-mtime pair keeps slot order (same_a before same_b) on every page
    let by_date = get(&ix, 0, usize::MAX, Order::MtimeDesc);
    assert_eq!(
        by_date.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["new_big.txt", "same_a.txt", "same_b.txt", "mid.txt", "old_big.txt", "old_small.txt"]
    );
    let p0 = get(&ix, 0, 2, Order::MtimeDesc);
    let p1 = get(&ix, 2, 2, Order::MtimeDesc);
    let p2 = get(&ix, 4, 2, Order::MtimeDesc);
    assert_eq!([p0, p1, p2].concat(), by_date, "pages must tile the full order exactly");
    assert_eq!(ix.search_str(q, &SearchOptions { offset: 6, limit: 5, order: Order::MtimeDesc }).items, vec![]);

    // largest first
    let by_size = get(&ix, 0, usize::MAX, Order::SizeDesc);
    assert_eq!(
        by_size.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["new_big.txt", "old_big.txt", "mid.txt", "same_a.txt", "same_b.txt", "old_small.txt"]
    );
    let (_, page) = {
        let r = ix.search_str(q, &SearchOptions { offset: 1, limit: 3, order: Order::SizeDesc });
        (r.total, r.items.iter().map(|&i| ix.name(i).to_string()).collect::<Vec<_>>())
    };
    assert_eq!(page, ["old_big.txt", "mid.txt", "same_a.txt"]);

    // a scoped query keeps the same order within the matches (dirty index path)
    ix.insert(C, 106, 5, "fresh.txt", false, 5, 600);
    let dirty = get(&ix, 0, usize::MAX, Order::MtimeDesc);
    assert_eq!(dirty[0].0, "fresh.txt");
    assert_eq!(dirty.len(), 7);
    ix.refresh();
    assert_eq!(get(&ix, 0, usize::MAX, Order::MtimeDesc), dirty);

    // a volume without metadata sorts as size 0 / oldest, never panics
    let mut nb = IndexBuilder::new(false);
    nb.add_volume(C, 5);
    nb.push(C, 200, 5, "x.txt", false, 3, 3);
    let no_meta = nb.finish();
    let r = no_meta.search_str(q, &SearchOptions { offset: 0, limit: 10, order: Order::MtimeDesc });
    assert_eq!(r.total, 1);
    assert_eq!(no_meta.name(r.items[0]), "x.txt");
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

#[test]
fn synthetic_ids_and_refs_interner() {
    let mut b = IndexBuilder::new(true);
    b.add_volume(b'R', 5);
    b.volume_mut(b'R').unwrap().kind = VolKind::Refs;
    let root128 = 0xdead_0000_0000_0000_0000_0000_0000_0001u128;
    b.set_id128(b'R', root128, 5);
    let a128 = (7u128 << 64) | 0x100;
    let a = b.intern128(b'R', a128);
    assert_eq!(a, FIRST_SYNTHETIC_ID);
    assert_eq!(b.intern128(b'R', a128), a, "stable");
    b.push(b'R', a, 5, "A", true, 0, 0);
    b.push(b'R', 0, a, "f.txt", false, 1, 2);
    let gone = b.intern128(b'R', 99); // interned, folder never pushed except as a stub parent
    let mut ix = b.finish();
    assert_eq!(ix.lookup128(b'R', root128), Some(5));
    assert_eq!(ix.lookup128(b'R', 12345), None);
    assert_eq!(ix.intern128(b'R', 12345), gone + 1, "continues from next_id");
    assert_eq!(ix.find_path(b'R', r"A\f.txt").map(|i| ix.full_path(i)), Some(r"R:\A\f.txt".into()));
    assert_eq!(ix.find_path(b'R', ""), ix.root_of(b'R'));
    assert_eq!(ix.find_path(b'R', r"A\nope"), None);
    // Folder volume: alloc_id adds the volume and never reuses ids
    assert_eq!(ix.alloc_id(b'F'), FIRST_SYNTHETIC_ID);
    assert_eq!(ix.alloc_id(b'F'), FIRST_SYNTHETIC_ID + 1);
    assert!(ix.root_of(b'F').is_some());
}

#[test]
fn snapshot_v2_keeps_kinds_ids_and_live_interned_folders() {
    let mut ix = sample();
    let r = ix.add_volume(b'R', 5);
    let _ = r;
    ix.volume_mut(b'R').unwrap().kind = VolKind::Refs;
    ix.set_id128(b'R', 1, 5);
    let a = ix.intern128(b'R', 0xAAAA_0000_0000_0000_0000_0000_0000_0000);
    ix.insert(b'R', a, 5, "kept", true, 0, 0);
    let b2 = ix.intern128(b'R', 0xBBBB);
    ix.insert(b'R', b2, 5, "removed", true, 0, 0);
    ix.remove_folder(b'R', b2);
    ix.alloc_id(b'F');
    ix.volume_mut(b'F').unwrap().kind = VolKind::Folder;
    let ld = Index::from_bytes(&ix.to_bytes()).unwrap();
    let kinds: Vec<_> = ld.volumes().iter().map(|v| (v.letter, v.kind, v.next_id)).collect();
    assert!(kinds.contains(&(C, VolKind::Ntfs, 0)));
    assert!(kinds.contains(&(b'R', VolKind::Refs, b2 + 1)));
    assert!(kinds.contains(&(b'F', VolKind::Folder, FIRST_SYNTHETIC_ID + 1)));
    assert_eq!(ld.lookup128(b'R', 0xAAAA_0000_0000_0000_0000_0000_0000_0000), Some(a));
    assert_eq!(ld.lookup128(b'R', 1), Some(5), "root is a live folder");
    assert_eq!(ld.lookup128(b'R', 0xBBBB), None, "dead folders are not persisted");
    assert_eq!(ld.folder_idx(b'R', a).map(|i| ld.full_path(i)), Some(r"R:\kept".into()));
}

#[test]
fn snapshot_v1_migrates_as_ntfs() {
    let ix = sample(); // one volume, no interned ids
    let v2 = ix.to_bytes();
    // v2 -> v1: drop kind u8 + next_id u64 after the 29-byte volume entry and the empty ids
    // section (u32 0) before the checksum, set version 1, re-checksum.
    let body = &v2[..v2.len() - 8];
    let mut v1 = Vec::new();
    v1.extend_from_slice(&body[..4]);
    v1.extend_from_slice(&1u32.to_le_bytes());
    v1.extend_from_slice(&body[8..24 + 29]);
    v1.extend_from_slice(&body[24 + 29 + 9..body.len() - 4]);
    assert_eq!(&body[body.len() - 4..], &0u32.to_le_bytes());
    let ck = super::snapshot::checksum_for_tests(&v1);
    v1.extend_from_slice(&ck.to_le_bytes());
    let ld = Index::from_bytes(&v1).unwrap();
    assert_eq!(ld.volumes()[0].kind, VolKind::Ntfs);
    assert_eq!(ld.volumes()[0].next_id, 0);
    assert_eq!(names(&ld, "report"), names(&ix, "report"));
    // unknown kind byte is rejected
    let mut bad = v2[..v2.len() - 8].to_vec();
    bad[24 + 29] = 9;
    let ck = super::snapshot::checksum_for_tests(&bad);
    bad.extend_from_slice(&ck.to_le_bytes());
    assert!(Index::from_bytes(&bad).is_err());
}

/// Incremental refresh (small change sets) must give the same order and ranks as a full rebuild.
#[test]
fn incremental_refresh_matches_full_rebuild() {
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = move |n: u64| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % n
    };
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    for d in 0..40u64 {
        b.push(C, 1000 + d, if d < 5 { 5 } else { 1000 + d % 5 }, &format!("dir{}", d * 7 % 13), true, 0, 0);
    }
    for f in 0..2000u64 {
        b.push(C, 10_000 + f, 1000 + f % 40, &format!("f{}.txt", f * 31 % 997), false, f, f as i64);
    }
    let mut ix = b.finish();
    let mut next = 50_000u64;
    for round in 0..60 {
        for _ in 0..1 + rnd(30) {
            let live: Vec<u32> = ix.search_str("", &SearchOptions::default()).items;
            match rnd(3) {
                0 => {
                    next += 1;
                    ix.insert(C, next, 1000 + rnd(40), &format!("n{}.dat", rnd(500)), false, 1, 1);
                }
                1 if !live.is_empty() => {
                    let i = live[rnd(live.len() as u64) as usize];
                    if !ix.is_dir(i) {
                        ix.remove(i);
                    }
                }
                _ if !live.is_empty() => {
                    let i = live[rnd(live.len() as u64) as usize];
                    if !ix.is_dir(i) {
                        let p = ix.folder_idx(C, 1000 + rnd(40)).unwrap();
                        ix.rename(i, p, &format!("r{}.bin", rnd(300)));
                    }
                }
                _ => {}
            }
        }
        // a search on the dirty index sees the same thing as after the refresh
        let dirty = names(&ix, "");
        ix.refresh();
        let inc_order = ix.order.clone();
        let inc_rank: Vec<u32> = (0..ix.parent.len()).map(|r| ix.rank.get(r).copied().unwrap_or(NONE)).collect();
        assert_eq!(names(&ix, ""), dirty, "round {round}");
        ix.rebuild_order();
        assert_eq!(ix.order, inc_order, "round {round}: order");
        let full_rank: Vec<u32> = (0..ix.parent.len()).map(|r| ix.rank[r]).collect();
        assert_eq!(full_rank, inc_rank, "round {round}: rank");
    }
}

#[test]
fn match_sets_page_anywhere_and_reverse_exactly() {
    // Enough rows to span many bitmap words and directory blocks, pushed out of name order.
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    let n = 5000u64;
    for k in 0..n {
        let j = (k * 2654435761) % n; // scrambled
        let name = if j.is_multiple_of(3) { format!("skip_{j:05}.bin") } else { format!("hit_{j:05}.txt") };
        b.push(C, 100 + k, 5, &name, false, j % 97, (j % 89) as i64);
    }
    let ix = b.finish();
    let q = Query::parse("hit");
    let all = ix.search(&q, &SearchOptions::default());
    assert_eq!(all.total, all.items.len());
    assert_eq!(ix.count(&q), all.total);
    assert_eq!(ix.count(&Query::parse("")), ix.order_len());
    let names: Vec<&str> = all.items.iter().map(|&i| ix.name(i)).collect();
    assert!(names.windows(2).all(|w| w[0] < w[1]), "name order");

    // name windows from one match set tile the full list, forwards and backwards
    let set = ix.name_matches(&q);
    assert_eq!(set.total(), all.total);
    for (off, lim) in [(0, 1), (0, 400), (63, 2), (511, 3), (512, 700), (all.total - 5, 50), (all.total, 9), (all.total + 9, 9)] {
        let want: Vec<u32> = all.items.iter().skip(off).take(lim).copied().collect();
        assert_eq!(ix.name_page(&set, off, lim, false), want, "asc window {off}+{lim}");
        let want: Vec<u32> = all.items.iter().rev().skip(off).take(lim).copied().collect();
        assert_eq!(ix.name_page(&set, off, lim, true), want, "desc window {off}+{lim}");
    }
    // the empty query pages straight off the order array
    let every = ix.name_matches(&Query::parse(""));
    assert_eq!(every.total(), ix.order_len());
    assert_eq!(ix.name_page(&every, 4000, 3, false), ix.search_str("", &SearchOptions { offset: 4000, limit: 3, order: Order::NameAsc }).items);

    // meta: the sorted list, its top-k prefix and the windowed search agree; asc is the exact reverse
    for (key, desc, asc) in [(MetaKey::Mtime, Order::MtimeDesc, Order::MtimeAsc), (MetaKey::Size, Order::SizeDesc, Order::SizeAsc)] {
        let sorted = ix.sort_slots(ix.match_slots(&q), key, None);
        assert_eq!(sorted.len(), all.total);
        assert_eq!(ix.top_slots(&ix.match_slots(&q), key, 300, None), sorted[..300]);
        assert_eq!(ix.search(&q, &SearchOptions { offset: 1000, limit: 40, order: desc }).items, sorted[1000..1040]);
        let rev: Vec<u32> = sorted.iter().rev().copied().collect();
        assert_eq!(ix.search(&q, &SearchOptions { offset: 0, limit: usize::MAX, order: asc }).items, rev);
        assert_eq!(ix.search(&q, &SearchOptions { offset: 7, limit: 5, order: asc }).items, rev[7..12]);
    }
}

#[test]
fn size_and_date_filters_and_folder_totals() {
    const EPOCH: i64 = 116_444_736_000_000_000;
    let ft = |ms: i64| ms * 10_000 + EPOCH;
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    b.push(C, 10, 5, "Photos", true, 0, ft(1_000));
    b.push(C, 11, 10, "2024", true, 0, ft(2_000));
    b.push(C, 12, 5, "Empty", true, 0, ft(3_000));
    b.push(C, 100, 11, "a.jpg", false, 3 << 20, ft(5_000));
    b.push(C, 101, 11, "b.jpg", false, 1 << 20, ft(6_000));
    b.push(C, 102, 10, "c.jpg", false, 500, ft(7_000));
    b.push(C, 103, 5, "d.txt", false, 0, ft(8_000));
    b.push(C, 104, 5, "size:huge.txt", false, 2048, ft(9_000));
    let ix = b.finish();
    let names = |q: &str| -> Vec<String> { ix.search_str(q, &SearchOptions::default()).items.iter().map(|&i| ix.name(i).to_string()).collect() };

    assert_eq!(names("size:>1mb"), ["a.jpg"]);
    assert_eq!(names("size:>=1mb"), ["a.jpg", "b.jpg"]);
    assert_eq!(names("size:<1kb"), ["c.jpg", "d.txt"], "folders never match a size filter");
    assert_eq!(names("size:0"), ["d.txt"]);
    assert_eq!(names("size:1kb..1.5mb"), ["b.jpg", "size:huge.txt"]);
    assert_eq!(names("jpg !size:>2mb"), ["b.jpg", "c.jpg"]);
    assert_eq!(names("size:huge"), ["size:huge.txt"], "an unparsable value is plain text");
    // dates: [from, to) in unix ms, open ends allowed, folders included
    assert_eq!(names("dm:5000..7000"), ["a.jpg", "b.jpg"]);
    assert_eq!(names("dm:8000.."), ["d.txt", "size:huge.txt"]);
    assert_eq!(names("dm:..2001"), ["2024", "Photos"]);
    assert_eq!(names("jpg dm:6000.. size:<1mb"), ["c.jpg"]);
    assert_eq!(ix.count(&Query::parse("size:>=1mb")), 2);

    // folder totals: every descendant counts, files and empty folders are 0
    let tot = ix.folder_totals();
    let of = |name: &str| tot[ix.search_str(name, &SearchOptions::default()).items[0] as usize];
    assert_eq!(of("2024"), 4 << 20);
    assert_eq!(of("Photos"), (4 << 20) + 500);
    assert_eq!(of("Empty"), 0);
    assert_eq!(of("a.jpg"), 0);
    // with the totals a size sort ranks folders by what they hold
    let sorted = ix.sort_slots(ix.match_slots(&Query::parse("")), MetaKey::Size, Some(&tot));
    let top: Vec<&str> = sorted.iter().map(|&i| ix.name(i)).filter(|n| !n.is_empty() && !n.ends_with(':')).take(4).collect();
    assert!(top.contains(&"Photos") && top.contains(&"2024") && top.contains(&"a.jpg"), "{top:?}");
    assert_eq!(ix.top_slots(&ix.match_slots(&Query::parse("")), MetaKey::Size, 3, Some(&tot)), sorted[..3]);
}

#[test]
fn type_order_groups_by_extension_then_name() {
    let mut b = IndexBuilder::new(true);
    b.add_volume(C, 5);
    b.push(C, 10, 5, "Docs.v2", true, 0, 0);
    for (k, n) in ["b.TXT", "a.txt", "z.jpg", "noext", "m.JPG", "c.md"].iter().enumerate() {
        b.push(C, 100 + k as u64, 5, n, false, 1, 0);
    }
    let ix = b.finish();
    let q = Query::parse("");
    let names = |order| -> Vec<String> {
        ix.search(&q, &SearchOptions { offset: 0, limit: 100, order }).items.iter().map(|&i| ix.name(i).to_string()).collect()
    };
    // folders lead (a dot in a folder name is not an extension), then no extension, then jpg < md < txt, names within a type
    assert_eq!(names(Order::TypeAsc), ["Docs.v2", "noext", "m.JPG", "z.jpg", "c.md", "a.txt", "b.TXT"]);
    let mut rev = names(Order::TypeAsc);
    rev.reverse();
    assert_eq!(names(Order::TypeDesc), rev);
}
