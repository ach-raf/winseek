#![cfg(windows)]

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use winseek_folder::*;

/// End-to-end check on a real FAT32/exFAT volume.
///
/// Create the volume once (elevated `cmd`/PowerShell; `New-VHD` is Hyper-V only, so use diskpart):
///
/// ```text
/// diskpart
///   create vdisk file=%TEMP%\ws-fat.vhdx maximum=256 type=expandable
///   select vdisk file=%TEMP%\ws-fat.vhdx
///   attach vdisk
///   create partition primary
///   format fs=fat32 quick label=WSFAT      (or fs=exfat)
///   assign letter=R
/// ```
///
/// Then run UNELEVATED:
///
/// ```text
/// set WINSEEK_TEST_FAT_ROOT=R:\ && cargo test -p winseek-folder -- --ignored
/// ```
///
/// Detach afterwards: `select vdisk file=%TEMP%\ws-fat.vhdx` then `detach vdisk`.
#[test]
#[ignore]
fn fat_volume_end_to_end() {
    let root = std::env::var("WINSEEK_TEST_FAT_ROOT").expect("set WINSEEK_TEST_FAT_ROOT, e.g. R:\\");
    let letter = root.chars().next().unwrap().to_ascii_uppercase();
    let vol = list_volumes().into_iter().find(|v| v.letter == letter).expect("volume not listed");
    assert!(vol.fs == "FAT32" || vol.fs == "exFAT", "unexpected fs {}", vol.fs);

    let base = format!("{}\\ws-test-{}", root.trim_end_matches('\\'), std::process::id());
    std::fs::create_dir_all(format!("{base}\\pre")).unwrap();
    std::fs::write(format!("{base}\\pre\\seed.txt"), b"seed").unwrap();

    let stop = AtomicBool::new(false);
    let mut next = 16u64;
    let mut names = Vec::new();
    let st = walk_names(&root, 5, &mut next, &stop, &mut |e| names.push((e.name.to_string(), e.is_dir))).unwrap();
    assert!(st.folders >= 2 && st.files >= 1);
    assert!(names.iter().any(|(n, d)| n == "seed.txt" && !*d));

    let mut w = DirWatch::open(&root).unwrap();
    let prefix = base[root.trim_end_matches('\\').len() + 1..].to_string();
    std::fs::create_dir(format!("{base}\\sub")).unwrap();
    std::fs::write(format!("{base}\\sub\\a.txt"), b"a").unwrap();
    std::fs::rename(format!("{base}\\sub\\a.txt"), format!("{base}\\sub\\b.txt")).unwrap();
    std::fs::write(format!("{base}\\sub\\c.txt"), b"c").unwrap();
    std::fs::remove_file(format!("{base}\\sub\\c.txt")).unwrap();

    let want = vec![
        PathEvent::Added(format!("{prefix}\\sub")),
        PathEvent::Added(format!("{prefix}\\sub\\a.txt")),
        PathEvent::Renamed { old: format!("{prefix}\\sub\\a.txt"), new: format!("{prefix}\\sub\\b.txt") },
        PathEvent::Removed(format!("{prefix}\\sub\\c.txt")),
    ];
    let mut all = Vec::new();
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end && !want.iter().all(|e| all.contains(e)) {
        if let Poll::Events(ev) = w.poll(200).unwrap() {
            all.extend(ev);
        }
    }
    drop(w);
    let _ = std::fs::remove_dir_all(&base);
    for e in &want {
        assert!(all.contains(e), "missing {e:?} in {all:?}");
    }
}
