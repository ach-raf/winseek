//! Download + extract a pinned llama.cpp build into
//! `<root>/llama/<build>/<backend>/`, keeping previous builds for rollback.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::fetch::{fetch_with, rewrite_hf, FetchOptions, Progress};
use crate::manifest::Manifest;
use crate::{Error, Result};

pub fn install_dir(root: &Path, build: &str, backend: Backend) -> PathBuf {
    root.join("llama").join(build).join(backend.dir_name())
}

/// Extract `zip_path` into `dest_dir` (created). Rejects entries escaping `dest_dir`.
pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let mut z = zip::ZipArchive::new(File::open(zip_path)?).map_err(|e| Error::Zip(e.to_string()))?;
    fs::create_dir_all(dest_dir)?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| Error::Zip(e.to_string()))?;
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| Error::Zip(format!("unsafe path in archive: {}", entry.name())))?;
        let out = dest_dir.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            io::copy(&mut entry, &mut File::create(&out)?)?;
        }
    }
    Ok(())
}

/// Locate `llama-server.exe` (or `llama-server`) under an install dir.
pub fn find_server_exe(dir: &Path) -> Option<PathBuf> {
    fn walk(d: &Path, depth: u32) -> Option<PathBuf> {
        for e in fs::read_dir(d).ok()?.flatten() {
            let p = e.path();
            if p.is_file() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n == "llama-server.exe" || n == "llama-server" {
                    return Some(p);
                }
            } else if p.is_dir() && depth < 3 {
                if let Some(f) = walk(&p, depth + 1) {
                    return Some(f);
                }
            }
        }
        None
    }
    walk(dir, 0)
}

/// Download (resumable, verified) and extract the manifest's asset for `backend`.
/// Idempotent: returns the existing exe when already installed. Extraction goes
/// to a temp dir first so a crash never leaves a half-populated install.
pub fn install_llama(
    root: &Path,
    manifest: &Manifest,
    backend: Backend,
    progress: &mut dyn FnMut(&str, Progress),
) -> Result<PathBuf> {
    install_llama_with(root, manifest, backend, &FetchOptions::default(), progress)
}

/// [`install_llama`] with explicit fetch options (retry policy, cancel flag). A cancelled
/// download returns [`Error::Cancelled`] and keeps its `.part` file for resume.
pub fn install_llama_with(
    root: &Path,
    manifest: &Manifest,
    backend: Backend,
    opts: &FetchOptions,
    progress: &mut dyn FnMut(&str, Progress),
) -> Result<PathBuf> {
    let build = &manifest.llama.build;
    let dir = install_dir(root, build, backend);
    if let Some(exe) = find_server_exe(&dir) {
        return Ok(exe);
    }
    let asset = manifest
        .asset(backend)
        .ok_or_else(|| Error::NotFound(format!("asset for {backend:?} in manifest")))?;
    let dl = root.join("downloads");
    let mut zips: Vec<(&str, &str)> = vec![(&asset.url, &asset.sha256)];
    zips.extend(asset.extra.iter().map(|e| (e.url.as_str(), e.sha256.as_str())));

    let tmp = dir.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    for (url, sha) in zips {
        let name = url.rsplit('/').next().unwrap_or("asset.zip");
        let dest = dl.join(name);
        let ep = std::env::var("WINSEEK_HF_ENDPOINT").ok();
        fetch_with(&[rewrite_hf(url, ep.as_deref())], &dest, sha, opts, &mut |p| progress(name, p))?;
        if opts.is_cancelled() {
            return Err(Error::Cancelled);
        }
        extract_zip(&dest, &tmp)?;
    }
    if find_server_exe(&tmp).is_none() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(Error::NotFound("llama-server in extracted archive".into()));
    }
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&tmp, &dir)?;
    find_server_exe(&dir).ok_or_else(|| Error::NotFound("llama-server after install".into()))
}

/// Installed build names (`bNNNN`) for `backend`, newest first.
pub fn installed_builds(root: &Path, backend: Backend) -> Vec<String> {
    let mut v: Vec<(u64, String)> = fs::read_dir(root.join("llama"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name.strip_prefix('b')?.parse::<u64>().ok()?;
            find_server_exe(&e.path().join(backend.dir_name())).map(|_| (n, name))
        })
        .collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.0));
    v.into_iter().map(|x| x.1).collect()
}

/// Exe for the pinned build if installed, else the newest older installed build (rollback).
pub fn resolve_exe(root: &Path, manifest: &Manifest, backend: Backend) -> Option<PathBuf> {
    if let Some(e) = find_server_exe(&install_dir(root, &manifest.llama.build, backend)) {
        return Some(e);
    }
    installed_builds(root, backend)
        .first()
        .and_then(|b| find_server_exe(&install_dir(root, b, backend)))
}

/// Delete all builds except the pinned one and the single most recent other one.
pub fn prune_builds(root: &Path, pinned: &str) -> io::Result<()> {
    let mut others: Vec<(u64, PathBuf)> = fs::read_dir(root.join("llama"))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == pinned {
                return None;
            }
            Some((name.strip_prefix('b')?.parse::<u64>().ok()?, e.path()))
        })
        .collect();
    others.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, p) in others.into_iter().skip(1) {
        fs::remove_dir_all(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("winseek-inst-{tag}-{}-{t}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, c) in files {
            w.start_file(*n, opts).unwrap();
            w.write_all(c).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn extract_and_find() {
        let d = tmp("ex");
        let z = d.join("a.zip");
        make_zip(&z, &[("llama-server.exe", b"MZ"), ("sub/ggml.dll", b"x")]);
        let out = d.join("out");
        extract_zip(&z, &out).unwrap();
        assert!(out.join("sub").join("ggml.dll").is_file());
        assert_eq!(find_server_exe(&out).unwrap(), out.join("llama-server.exe"));
    }

    #[test]
    fn zip_slip_rejected() {
        let d = tmp("slip");
        let z = d.join("a.zip");
        make_zip(&z, &[("../evil.txt", b"x")]);
        assert!(matches!(extract_zip(&z, &d.join("out")), Err(Error::Zip(_))));
        assert!(!d.join("evil.txt").exists());
    }

    #[test]
    fn rollback_and_prune() {
        let root = tmp("rb");
        for b in ["b100", "b200", "b300"] {
            let dir = install_dir(&root, b, Backend::Cpu);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("llama-server.exe"), b"x").unwrap();
        }
        assert_eq!(installed_builds(&root, Backend::Cpu), ["b300", "b200", "b100"]);
        let mut m = Manifest::embedded();
        m.llama.build = "b400".into(); // pinned build not installed -> newest older
        assert_eq!(resolve_exe(&root, &m, Backend::Cpu).unwrap(), install_dir(&root, "b300", Backend::Cpu).join("llama-server.exe"));
        assert!(resolve_exe(&root, &m, Backend::Vulkan).is_none());
        prune_builds(&root, "b400").unwrap();
        assert_eq!(installed_builds(&root, Backend::Cpu), ["b300"]);
    }
}
