//! Disk-usage roll-up for one folder: what each child holds (bytes, files, folders), the
//! file types under it and its biggest files. One linear pass over the record arrays; the
//! child each record sits under is memoised along the parent chain, so the pass is O(records).

use crate::store::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::BuildHasherDefault;

const UNK: u32 = u32::MAX;
const OUT: u32 = u32::MAX - 1;
const MAX_DEPTH: usize = 4096;
const EXT_MAX: usize = 8;

/// What one direct child of the folder holds (a file is its own bucket).
#[derive(Debug, Clone, PartialEq)]
pub struct ChildUsage {
    pub slot: u32,
    pub is_dir: bool,
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
    /// Bytes per file category ([`category`]); a file has all of its bytes in one.
    pub cats: [u64; CATEGORIES],
}

impl ChildUsage {
    /// The category holding most of the bytes (0 = other when empty).
    pub fn kind(&self) -> u8 {
        let mut best = 0usize;
        for k in 1..CATEGORIES {
            if self.cats[k] > self.cats[best] {
                best = k;
            }
        }
        best as u8
    }
}

/// File categories the disk-usage view colours by.
pub const CATEGORIES: usize = 8;
/// 0 other, 1 pictures, 2 video, 3 audio, 4 documents, 5 archives, 6 code, 7 programs and system.
pub fn category(ext: &str) -> u8 {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "svg" | "ico" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" | "avif" | "jxl" => 1,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "ts" | "m2ts" | "vob" | "3gp" => 2,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "aiff" | "mid" => 3,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "ods" | "csv" | "epub" | "mobi" | "djvu" | "log" => 4,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "img" | "cab" | "wim" | "vhd" | "vhdx" | "vmdk" | "zst" | "esd" => 5,
        "rs" | "py" | "js" | "jsx" | "tsx" | "java" | "c" | "h" | "cpp" | "hpp" | "cs" | "go" | "rb" | "php" | "html" | "css" | "json" | "xml" | "yml" | "yaml" | "toml" | "sql" | "sh" | "ps1" | "bat" | "kt" | "swift" | "map" | "pyc" | "o" | "obj" | "rlib" | "pdb" | "lock" => 6,
        "exe" | "dll" | "sys" | "msi" | "msix" | "appx" | "dat" | "bin" | "lib" | "mui" | "mum" | "manifest" | "cat" | "drv" | "ocx" | "efi" | "etl" | "db" | "sqlite" => 7,
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeUsage {
    /// Lower-case extension without the dot; empty = no extension.
    pub ext: String,
    pub kind: u8,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderUsage {
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
    /// Every direct child, biggest first.
    pub children: Vec<ChildUsage>,
    /// File types under the folder, biggest first.
    pub types: Vec<TypeUsage>,
    /// Biggest files anywhere under the folder, biggest first: `(slot, bytes)`.
    pub biggest: Vec<(u32, u64)>,
}

fn pack_ext(name: &[u8]) -> u64 {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else { return 0 };
    let e = &name[dot + 1..];
    if e.is_empty() || e.len() > EXT_MAX {
        return 0;
    }
    let mut k = 0u64;
    for (j, &b) in e.iter().enumerate() {
        k |= (b.to_ascii_lowercase() as u64) << (8 * j);
    }
    k
}

fn unpack_ext(k: u64) -> String {
    (0..EXT_MAX).map(|j| ((k >> (8 * j)) & 0xFF) as u8).take_while(|&b| b != 0).map(|b| b as char).collect()
}

impl Index {
    /// Resolve `C:\a\b` to a record slot without mutating the index (case-insensitive).
    /// `C:` / `C:\` is the volume root.
    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        let mut parts = path.split(['\\', '/']).filter(|p| !p.is_empty());
        let first = parts.next()?;
        let letter = first.chars().next()?.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() || !first[1..].starts_with(':') {
            return None;
        }
        let mut cur = self.root_of(letter as u8)?;
        for part in parts {
            let want = part.to_lowercase();
            let mut hit = None;
            for i in 0..self.parent.len() {
                if self.parent[i] == cur && self.flags[i] & F_DELETED == 0 && self.name(i as u32).to_lowercase() == want {
                    hit = Some(i as u32);
                    break;
                }
            }
            cur = hit?;
        }
        Some(cur)
    }

    /// Roll up everything under `cur` (a folder slot). `top_files` bounds the biggest-files list.
    /// Sizes are the logical sizes the index keeps; without meta everything is zero.
    pub fn folder_usage(&self, cur: u32, top_files: usize) -> FolderUsage {
        let n = self.parent.len();
        let vol = self.vol[cur as usize];
        let mut memo = vec![UNK; n];
        let mut chain: Vec<u32> = Vec::new();
        let mut acc: HashMap<u32, ChildUsage, BuildHasherDefault<FxHasher>> = HashMap::default();
        let mut types: HashMap<u64, (u64, u64), BuildHasherDefault<FxHasher>> = HashMap::default();
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
        let mut out = FolderUsage::default();

        for i in 0..n {
            let f = self.flags[i];
            if f & F_DELETED != 0 || self.vol[i] != vol || i as u32 == cur {
                continue;
            }
            // Which direct child of `cur` is this record under (or OUT)?
            chain.clear();
            let mut x = i as u32;
            let bucket = loop {
                let m = memo[x as usize];
                if m != UNK {
                    break m;
                }
                let p = self.parent[x as usize];
                if p == cur {
                    memo[x as usize] = x;
                    break x;
                }
                if p as usize >= n || p == x || chain.len() >= MAX_DEPTH {
                    memo[x as usize] = OUT;
                    break OUT;
                }
                chain.push(x);
                x = p;
            };
            for &c in &chain {
                memo[c as usize] = bucket;
            }
            if bucket == OUT {
                continue;
            }
            let is_dir = f & F_DIR != 0;
            let e = acc.entry(bucket).or_insert_with(|| ChildUsage {
                slot: bucket,
                is_dir: self.flags[bucket as usize] & F_DIR != 0,
                bytes: 0,
                files: 0,
                folders: 0,
                cats: [0; CATEGORIES],
            });
            if is_dir {
                if i as u32 != bucket {
                    e.folders += 1;
                    out.folders += 1;
                }
                if i as u32 == bucket {
                    out.folders += 1;
                }
            } else {
                let sz = if self.with_meta { self.size[i] } else { 0 };
                e.bytes = e.bytes.saturating_add(sz);
                e.files += 1;
                e.cats[category(&unpack_ext(pack_ext(self.name_bytes(i as u32)))) as usize] += sz;
                out.bytes = out.bytes.saturating_add(sz);
                out.files += 1;
                let t = types.entry(pack_ext(self.name_bytes(i as u32))).or_default();
                t.0 = t.0.saturating_add(sz);
                t.1 += 1;
                if top_files > 0 && sz > 0 {
                    if heap.len() < top_files {
                        heap.push(Reverse((sz, i as u32)));
                    } else if heap.peek().is_some_and(|r| sz > r.0 .0) {
                        heap.pop();
                        heap.push(Reverse((sz, i as u32)));
                    }
                }
            }
        }
        // Folders with nothing in them still are children.
        for i in 0..n {
            if self.parent[i] == cur && self.flags[i] & F_DELETED == 0 && i as u32 != cur {
                acc.entry(i as u32).or_insert_with(|| ChildUsage {
                    slot: i as u32,
                    is_dir: self.flags[i] & F_DIR != 0,
                    bytes: 0,
                    files: 0,
                    folders: 0,
                    cats: [0; CATEGORIES],
                });
            }
        }
        out.children = acc.into_values().collect();
        out.children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(b.files.cmp(&a.files)).then(a.slot.cmp(&b.slot)));
        let mut tv: Vec<TypeUsage> =
            types.into_iter().map(|(k, (bytes, files))| { let ext = unpack_ext(k); TypeUsage { kind: category(&ext), ext, bytes, files } }).collect();
        tv.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.ext.cmp(&b.ext)));
        out.types = tv;
        let mut bf: Vec<(u32, u64)> = heap.into_iter().map(|Reverse((s, i))| (i, s)).collect();
        bf.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.biggest = bf;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'C', 5);
        b.push(b'C', 10, 5, "Users", true, 0, 0);
        b.push(b'C', 11, 10, "Pics", true, 0, 0);
        b.push(b'C', 12, 11, "a.JPG", false, 300, 0);
        b.push(b'C', 13, 11, "b.jpg", false, 200, 0);
        b.push(b'C', 14, 10, "notes.txt", false, 50, 0);
        b.push(b'C', 15, 5, "pagefile.sys", false, 1000, 0);
        b.push(b'C', 16, 10, "Empty", true, 0, 0);
        b.finish()
    }

    #[test]
    fn rolls_up_children_types_and_biggest() {
        let ix = sample();
        let users = ix.resolve_path(r"c:\users").unwrap();
        let u = ix.folder_usage(users, 10);
        assert_eq!((u.bytes, u.files, u.folders), (550, 3, 2));
        let names: Vec<_> = u.children.iter().map(|c| (ix.name(c.slot).to_string(), c.bytes, c.files, c.folders)).collect();
        assert_eq!(names[0], ("Pics".into(), 500, 2, 0));
        assert_eq!(names[1], ("notes.txt".into(), 50, 1, 0));
        assert!(names.iter().any(|n| n.0 == "Empty" && n.1 == 0));
        assert_eq!(u.types[0], TypeUsage { ext: "jpg".into(), kind: 1, bytes: 500, files: 2 });
        assert_eq!(ix.name(u.biggest[0].0), "a.JPG");
    }

    #[test]
    fn volume_root_and_unknown_paths() {
        let ix = sample();
        let root = ix.resolve_path("C:\\").unwrap();
        let u = ix.folder_usage(root, 0);
        assert_eq!((u.bytes, u.files), (1550, 4));
        assert_eq!(ix.name(u.children[0].slot), "pagefile.sys");
        assert!(ix.resolve_path(r"C:\nope").is_none());
        assert!(ix.resolve_path(r"Z:\").is_none());
    }
}
