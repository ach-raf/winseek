//! Reader guard rails: what Tier 1 must never read, decided from the path and its attributes.
//!
//! - [`read_guarded`] / [`read_head`]: the only way the worker queues read file bytes. They
//!   refuse hard-denied (secret) paths and cloud placeholders *at read time* (a file can become
//!   a placeholder after it was crawled), checking attributes with `metadata` (opens with
//!   `FILE_READ_ATTRIBUTES` only, so nothing is recalled).
//! - [`looks_like_dataset`]: machine-learning dataset folders (thousands of numbered images,
//!   annotation sidecars, train/val/test splits) are noise, not a library.
//! - Code-project helpers: project markers, vendored / generated / secret-looking source files.

use std::io::{self, Read};
use std::path::Path;

pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// Reading the content would make a cloud provider (OneDrive, Dropbox, ...) download the file.
pub fn is_cloud_only(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

/// Marker text of the error returned for a cloud-only file (see [`is_cloud_error`]).
pub const CLOUD_ONLY: &str = "cloud_only";

pub fn is_cloud_error(e: &io::Error) -> bool {
    e.to_string() == CLOUD_ONLY
}

/// Why a failed guarded read is a permanent *skip* (not a failure): `cloud_only` / `denied`.
pub fn skip_reason(e: &io::Error) -> Option<&'static str> {
    if is_cloud_error(e) {
        Some(CLOUD_ONLY)
    } else if e.kind() == io::ErrorKind::PermissionDenied && e.to_string() == "denied" {
        Some("denied")
    } else {
        None
    }
}

/// `Ok` when the file may be read: not hard-denied, not a cloud placeholder.
pub fn check_readable(path: &Path) -> io::Result<()> {
    if winseek_store::hard_denied(&path.to_string_lossy()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    }
    use std::os::windows::fs::MetadataExt;
    let attrs = std::fs::metadata(path)?.file_attributes();
    if is_cloud_only(attrs) {
        return Err(io::Error::other(CLOUD_ONLY));
    }
    Ok(())
}

/// `std::fs::read` behind [`check_readable`].
pub fn read_guarded(path: &Path) -> io::Result<Vec<u8>> {
    check_readable(path)?;
    std::fs::read(path)
}

/// The first `n` bytes of a file, behind [`check_readable`].
pub fn read_head(path: &Path, n: usize) -> io::Result<Vec<u8>> {
    check_readable(path)?;
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------- files that are not finished

/// A file modified this recently may still be being written (a download, a copy, a save).
pub const FRESH_SECS: u64 = 120;

/// True when a file last modified at `mtime` may still be written to at `now` (or claims the future).
pub fn is_fresh_at(mtime: std::time::SystemTime, now: std::time::SystemTime) -> bool {
    now.duration_since(mtime).map(|d| d.as_secs() < FRESH_SECS).unwrap_or(true)
}

/// True when `path` was modified within [`FRESH_SECS`]. (Unit tests write their fixtures a moment
/// before reading them, so there it is always false.)
pub fn is_fresh(path: &str) -> bool {
    if cfg!(test) {
        return false;
    }
    std::fs::metadata(path).and_then(|m| m.modified()).map(|t| is_fresh_at(t, std::time::SystemTime::now())).unwrap_or(false)
}

/// Office lock files (`~$name.docx`) and download / temp leftovers: never content, whatever the extension.
pub fn is_temp_name(path: &str) -> bool {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("~$") {
        return true;
    }
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["part", "crdownload", "download", "partial", "tmp", "temp", "opdownload", "!ut", "bc!"].iter().any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// Fail a file that could not be read. A file still being written is not damaged yet: it is
/// skipped as `still_writing` and looked at again later.
pub fn fail_file(store: &std::sync::Mutex<winseek_store::Store>, id: winseek_store::FileId, path: &str, msg: &str) -> Result<(), String> {
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    if is_fresh(path) { s.skip_with(id, "still_writing", msg) } else { s.fail(id, msg) }.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- dataset folders

/// Folder names that are datasets by themselves.
const DATASET_NAMES: &[&str] = &[
    "dataset", "datasets", "data_set", "data-set", "imagenet", "ilsvrc*", "coco", "mscoco", "ms-coco", "mnist", "fashion-mnist", "fashion_mnist",
    "cifar*", "celeba*", "lfw", "kitti", "cityscapes", "openimages", "open-images", "pascal_voc", "pascalvoc", "voc20*", "vggface*", "ffhq*",
    "lsun", "tiny-imagenet*", "oxford-iiit-pet", "flowers102", "caltech101", "caltech256",
];
/// Files that annotate a labelled image set.
const ANNOTATION_FILES: &[&str] = &[
    "labels.csv", "labels.txt", "labels.json", "annotations.json", "annotations.csv", "_annotations.coco.json", "classes.txt", "classes.names",
    "data.yaml", "dataset.json", "dataset_info.json", "metadata.csv", "train.csv", "train.txt", "val.txt", "test.txt", "synsets.txt", "obj.names",
];
const SPLIT_DIRS: &[&str] = &["train", "training", "val", "valid", "validation", "test", "testing", "dev"];
/// Camera / phone / screenshot names are numbered too, but they are a person's library.
const CAMERA_PREFIXES: &[&str] = &[
    "img", "dsc", "dscn", "dscf", "pxl", "photo", "vid", "mvimg", "image", "screenshot", "screen", "capture", "snip", "wp", "pano", "burst", "p",
    "dji", "gopr", "gh", "gx",
];
const MIN_SEQUENTIAL_IMAGES: usize = 200;
const MIN_LABELLED_IMAGES: usize = 20;

fn dataset_names() -> &'static winseek_store::Ignore {
    static D: std::sync::OnceLock<winseek_store::Ignore> = std::sync::OnceLock::new();
    D.get_or_init(|| winseek_store::Ignore::new(DATASET_NAMES))
}

fn is_image_name(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    ["jpg", "jpeg", "png", "bmp", "gif", "webp", "tif", "tiff"].iter().any(|e| n.rsplit_once('.').is_some_and(|(_, x)| x == *e))
}

/// `000123`, `00123_left`, `frame_000123`, `im-0042`... but not `IMG_0042` / `DSC00123` / dated names.
fn sequential_stem(stem: &str) -> bool {
    let s = stem.to_ascii_lowercase();
    let digits_end = s.trim_end_matches(|c: char| c.is_ascii_digit());
    let nd = s.len() - digits_end.len();
    if nd >= 5 && digits_end.is_empty() {
        return !(nd == 8 && (s.starts_with("19") || s.starts_with("20"))); // 20240131 = a date
    }
    if nd < 4 {
        return false;
    }
    let word = digits_end.trim_end_matches(['_', '-', ' ', '.']);
    if word.is_empty() || word.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    !CAMERA_PREFIXES.contains(&word)
}

/// A directory whose own entries look like a machine-learning dataset. `names` = file names (not
/// directories) in it, `dirs` = its sub-directory names.
pub fn looks_like_dataset(dir_name: &str, names: &[String], dirs: &[String]) -> bool {
    if dataset_names().matches_name(dir_name) {
        return true;
    }
    let splits = dirs.iter().filter(|d| SPLIT_DIRS.contains(&d.to_ascii_lowercase().as_str())).count();
    if splits >= 2 {
        return true;
    }
    let images: Vec<&String> = names.iter().filter(|n| is_image_name(n)).collect();
    if images.len() >= MIN_LABELLED_IMAGES && names.iter().any(|n| ANNOTATION_FILES.contains(&n.to_ascii_lowercase().as_str())) {
        return true;
    }
    if images.len() >= MIN_SEQUENTIAL_IMAGES {
        let seq = images.iter().filter(|n| sequential_stem(n.rsplit_once('.').map_or(n.as_str(), |(s, _)| s))).count();
        return seq * 100 >= images.len() * 90;
    }
    false
}

// ---------------------------------------------------------------- code projects

const PROJECT_MARKER_FILES: &[&str] = &[
    "cargo.toml", "package.json", "pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "pipfile", "go.mod", "pom.xml", "build.gradle",
    "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "cmakelists.txt", "makefile", "composer.json", "gemfile", "mix.exs",
    "pubspec.yaml", "deno.json", "tsconfig.json", "stack.yaml", "dune-project", "build.sbt", "project.clj", "meson.build", "justfile",
];
const PROJECT_MARKER_SUFFIXES: &[&str] = &[".sln", ".csproj", ".fsproj", ".vbproj", ".vcxproj", ".xcodeproj", ".cabal", ".gemspec"];

/// True when a directory holding exactly these entry names is a code project root.
pub fn is_project_root(entry_names: &[String]) -> bool {
    entry_names.iter().any(|n| {
        let l = n.to_ascii_lowercase();
        l == ".git" || PROJECT_MARKER_FILES.contains(&l.as_str()) || PROJECT_MARKER_SUFFIXES.iter().any(|s| l.ends_with(s))
    })
}

fn dir_is_project_root(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    let names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    is_project_root(&names)
}

/// Some ancestor directory of `path` is a code project root. The drive root and the user profile
/// folder never count, so a dotfiles repo in a home folder does not turn every document into
/// "project content".
pub fn in_code_project(path: &str) -> bool {
    let profile = std::env::var("USERPROFILE").unwrap_or_default().trim_end_matches('\\').to_lowercase();
    let mut p = Path::new(path).parent();
    while let Some(d) = p {
        if d.components().count() <= 2 || (!profile.is_empty() && d.to_string_lossy().trim_end_matches('\\').to_lowercase() == profile) {
            return false;
        }
        if dir_is_project_root(d) {
            return true;
        }
        p = d.parent();
    }
    false
}

const VENDORED_DIRS: &[&str] = &[
    "vendor", "third_party", "third-party", "thirdparty", "external", "extern", "node_modules", "bower_components", "site-packages", "deps",
    "pods", "carthage", "generated", "__generated__", "dist", "build", "out", "target", "bin", "obj", ".next", ".nuxt",
];

/// Generated / vendored / secret-looking source files that never belong in the code index.
pub fn skip_code_file(path: &str) -> bool {
    let norm = path.replace('/', "\\");
    let mut parts: Vec<String> = norm.split('\\').map(|s| s.to_ascii_lowercase()).collect();
    let name = parts.pop().unwrap_or_default();
    if parts.iter().any(|d| VENDORED_DIRS.contains(&d.as_str())) {
        return true;
    }
    if winseek_store::hard_denied(path) {
        return true;
    }
    const GENERATED_SUFFIXES: &[&str] = &[
        ".min.js", ".min.css", ".map", ".d.ts", ".designer.cs", ".g.cs", ".g.i.cs", ".g.dart", ".freezed.dart", ".pb.go", "_pb2.py", "_pb2_grpc.py",
        ".pb.h", ".pb.cc", ".generated.ts", ".generated.cs", ".generated.go", ".bundle.js", ".chunk.js",
    ];
    const LOCKS: &[&str] = &[
        "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "cargo.lock", "poetry.lock", "pipfile.lock", "composer.lock", "gemfile.lock",
        "go.sum", "bun.lockb", "npm-shrinkwrap.json",
    ];
    if GENERATED_SUFFIXES.iter().any(|s| name.ends_with(s)) || name.contains(".min.") || LOCKS.contains(&name.as_str()) {
        return true;
    }
    // Secret-looking by name: .env, secrets, credentials, tokens, private keys.
    ["secret", "credential", "passwd", "private_key", "privatekey", "apikey", "api_key"]
        .iter()
        .any(|w| name.contains(w))
}

// ---------------------------------------------------------------- notebooks

/// Text of a Jupyter notebook: markdown + code cell *sources* only; outputs (huge base64 images,
/// logs, possibly secrets) are dropped. `None` when it is not a notebook.
pub fn notebook_text(json: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let cells = v.get("cells").and_then(|c| c.as_array()).or_else(|| {
        // nbformat 3: worksheets[0].cells
        v.get("worksheets")?.as_array()?.first()?.get("cells")?.as_array()
    })?;
    let mut out = String::new();
    for c in cells {
        let src = c.get("source").or_else(|| c.get("input"));
        let text = match src {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect::<String>(),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(&text);
        out.push_str("\n\n");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn files_written_a_moment_ago_are_fresh_and_temp_names_are_recognised() {
        use std::time::{Duration, SystemTime};
        let now = SystemTime::now();
        assert!(super::is_fresh_at(now - Duration::from_secs(5), now));
        assert!(super::is_fresh_at(now + Duration::from_secs(5), now));
        assert!(!super::is_fresh_at(now - Duration::from_secs(3600), now));
        for n in [r"C:\d\~$report.docx", r"C:\d\movie.mp4.part", r"C:\d\a.crdownload", r"C:\d\x.tmp"] {
            assert!(super::is_temp_name(n), "{n}");
        }
        assert!(!super::is_temp_name(r"C:\d\report.docx"));
    }

    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cloud_attributes_are_detected() {
        assert!(is_cloud_only(ATTR_RECALL_ON_DATA_ACCESS));
        assert!(is_cloud_only(ATTR_RECALL_ON_OPEN | 0x20));
        assert!(is_cloud_only(ATTR_OFFLINE));
        assert!(!is_cloud_only(0x20 | 0x400)); // plain archive file, reparse alone is not a recall
    }

    #[test]
    fn denied_and_local_files_at_read_time() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("a.txt");
        std::fs::write(&ok, b"hello").unwrap();
        assert_eq!(read_guarded(&ok).unwrap(), b"hello");
        assert_eq!(read_head(&ok, 3).unwrap(), b"hel");
        let key = d.path().join("server.pem");
        std::fs::write(&key, b"-----BEGIN").unwrap();
        assert_eq!(read_guarded(&key).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(read_head(&key, 3).is_err());
        let ssh = d.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        std::fs::write(ssh.join("notes.txt"), b"x").unwrap();
        assert!(read_guarded(&ssh.join("notes.txt")).is_err());
    }

    #[test]
    fn cloud_error_marker_round_trips() {
        let e = io::Error::other(CLOUD_ONLY);
        assert!(is_cloud_error(&e));
        assert!(!is_cloud_error(&io::Error::from(io::ErrorKind::NotFound)));
    }

    #[test]
    fn dataset_folders() {
        assert!(looks_like_dataset("ImageNet", &[], &[]));
        assert!(looks_like_dataset("cifar-10", &[], &[]));
        assert!(looks_like_dataset("x", &[], &names(&["train", "val", "docs"])));
        assert!(!looks_like_dataset("x", &[], &names(&["train", "photos"])));
        let seq: Vec<String> = (0..300).map(|i| format!("{i:06}.jpg")).collect();
        assert!(looks_like_dataset("faces", &seq, &[]));
        let frames: Vec<String> = (0..300).map(|i| format!("frame_{i:05}.png")).collect();
        assert!(looks_like_dataset("clip", &frames, &[]));
        let mut labelled: Vec<String> = (0..25).map(|i| format!("cat{i}.jpg")).collect();
        assert!(!looks_like_dataset("pets", &labelled, &[]));
        labelled.push("Labels.CSV".into());
        assert!(looks_like_dataset("pets", &labelled, &[]));
        // a person's camera roll is numbered too
        let cam: Vec<String> = (0..500).map(|i| format!("IMG_{i:04}.jpg")).collect();
        assert!(!looks_like_dataset("Camera Roll", &cam, &[]));
        let dated: Vec<String> = (0..300).map(|i| format!("2024{:02}{:02}.jpg", 1 + i % 12, 1 + i % 28)).collect();
        assert!(!looks_like_dataset("Trip", &dated, &[]));
        let few: Vec<String> = (0..50).map(|i| format!("{i:06}.jpg")).collect();
        assert!(!looks_like_dataset("scans", &few, &[]), "a short numbered run is not a dataset");
    }

    #[test]
    fn project_markers() {
        assert!(is_project_root(&names(&["src", "Cargo.toml"])));
        assert!(is_project_root(&names(&[".git", "a.txt"])));
        assert!(is_project_root(&names(&["App.sln"])));
        assert!(!is_project_root(&names(&["holiday.jpg", "notes.txt"])));
    }

    #[test]
    fn in_code_project_walks_up_but_not_to_the_profile() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("a").join("b").join("proj");
        let sub = proj.join("docs").join("img");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(proj.join("package.json"), b"{}").unwrap();
        let shot = sub.join("shot.png");
        assert!(in_code_project(&shot.to_string_lossy()));
        let outside = d.path().join("a").join("b").join("photos");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(!in_code_project(&outside.join("x.jpg").to_string_lossy()));
        assert!(!in_code_project(r"C:\x.png"));
    }

    #[test]
    fn code_files_to_skip() {
        for p in [
            r"D:\p\vendor\lib\a.go",
            r"D:\p\node_modules\x\i.js",
            r"D:\p\app.min.js",
            r"D:\p\a.pb.go",
            r"D:\p\x_pb2.py",
            r"D:\p\Cargo.lock",
            r"D:\p\src\secrets.py",
            r"D:\p\src\aws_credentials.js",
            r"D:\p\.env",
            r"D:\p\types.d.ts",
        ] {
            assert!(skip_code_file(p), "{p}");
        }
        for p in [r"D:\p\src\main.rs", r"D:\p\lib\parser.py", r"D:\p\README.md", r"D:\p\a.ipynb"] {
            assert!(!skip_code_file(p), "{p}");
        }
    }

    #[test]
    fn notebooks_read_without_outputs() {
        let nb = br##"{"cells":[
            {"cell_type":"markdown","source":["# Title\n","text"],"metadata":{}},
            {"cell_type":"code","source":"x = secret_df.head()","outputs":[{"data":{"text/plain":"LEAKED OUTPUT","image/png":"AAAA"}}]},
            {"cell_type":"code","source":[],"outputs":[]}
        ],"metadata":{},"nbformat":4}"##;
        let t = notebook_text(nb).unwrap();
        assert!(t.contains("# Title") && t.contains("secret_df.head()"));
        assert!(!t.contains("LEAKED") && !t.contains("AAAA"));
        assert!(notebook_text(b"not json").is_none());
        assert!(notebook_text(br#"{"a":1}"#).is_none());
    }
}
