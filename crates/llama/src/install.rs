//! Download + extract a pinned llama.cpp build into
//! `<root>/llama/<build>/<backend>/`, keeping previous builds for rollback.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::fetch::{fetch_with, rewrite_hf, FetchOptions, Progress};
use crate::manifest::{FfmpegManifest, Manifest, PdfiumManifest};
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

// ---------------------------------------------------------------- pdfium

/// `<root>/pdfium/<build>/<dll>`.
pub fn pdfium_path(root: &Path, p: &PdfiumManifest) -> PathBuf {
    root.join("pdfium").join(&p.build).join(p.dll_name())
}

/// Pull one member (`bin/pdfium.dll`) out of a `.tgz` into `dest`, via a temp file so a crash never
/// leaves a truncated DLL. Only the exact member path is read; nothing else is written.
pub fn extract_tgz_member(tgz: &Path, member: &str, dest: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(File::open(tgz)?);
    let mut ar = tar::Archive::new(gz);
    let want = member.trim_start_matches("./");
    for entry in ar.entries().map_err(|e| Error::Zip(format!("tgz: {e}")))? {
        let mut entry = entry.map_err(|e| Error::Zip(format!("tgz: {e}")))?;
        let name = entry.path().map_err(|e| Error::Zip(format!("tgz: {e}")))?.to_string_lossy().replace('\\', "/");
        if name.trim_start_matches("./") != want {
            continue;
        }
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = dest.with_extension("tmp");
        let res = File::create(&tmp).and_then(|mut f| io::copy(&mut entry, &mut f).map(|_| ()));
        if let Err(e) = res {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        let _ = fs::remove_file(dest);
        fs::rename(&tmp, dest)?;
        return Ok(());
    }
    Err(Error::NotFound(format!("{member} in {}", tgz.display())))
}

/// Download (resumable, sha256-verified) and unpack the pinned pdfium DLL. Idempotent: returns the
/// existing path when already installed.
pub fn install_pdfium_with(
    root: &Path,
    manifest: &Manifest,
    opts: &FetchOptions,
    progress: &mut dyn FnMut(&str, Progress),
) -> Result<PathBuf> {
    let p = &manifest.pdfium;
    let dll = pdfium_path(root, p);
    if dll.is_file() {
        return Ok(dll);
    }
    let name = p.url.rsplit('/').next().unwrap_or("pdfium.tgz");
    let dest = root.join("downloads").join(format!("pdfium-{}-{name}", p.build));
    fetch_with(std::slice::from_ref(&p.url), &dest, &p.sha256, opts, &mut |pr| progress(name, pr))?;
    if opts.is_cancelled() {
        return Err(Error::Cancelled);
    }
    extract_tgz_member(&dest, &p.member, &dll)?;
    Ok(dll)
}

// ---------------------------------------------------------------- ffmpeg

/// `<root>/ffmpeg/<build>/<exe>`.
pub fn ffmpeg_path(root: &Path, f: &FfmpegManifest) -> PathBuf {
    root.join("ffmpeg").join(&f.build).join(f.exe_name())
}

/// Pull one member out of a `.zip` into `dest` via a temp file (a crash never leaves a truncated
/// exe). Only the exact member path is read; nothing else is written.
pub fn extract_zip_member(zip_path: &Path, member: &str, dest: &Path) -> Result<()> {
    let mut z = zip::ZipArchive::new(File::open(zip_path)?).map_err(|e| Error::Zip(e.to_string()))?;
    let want = member.trim_start_matches("./");
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| Error::Zip(e.to_string()))?;
        if entry.is_dir() || entry.name().replace('\\', "/").trim_start_matches("./") != want {
            continue;
        }
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = dest.with_extension("tmp");
        let res = File::create(&tmp).and_then(|mut f| io::copy(&mut entry, &mut f).map(|_| ()));
        if let Err(e) = res {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        let _ = fs::remove_file(dest);
        fs::rename(&tmp, dest)?;
        return Ok(());
    }
    Err(Error::NotFound(format!("{member} in {}", zip_path.display())))
}

/// Download (resumable, sha256-verified) and unpack the pinned ffmpeg. Idempotent. An unpinned
/// manifest hash is refused by the fetch ([`Error::UnpinnedHash`]); a manifest without an ffmpeg
/// entry is `NotFound`.
pub fn install_ffmpeg_with(
    root: &Path,
    manifest: &Manifest,
    opts: &FetchOptions,
    progress: &mut dyn FnMut(&str, Progress),
) -> Result<PathBuf> {
    let f = manifest.ffmpeg.as_ref().ok_or_else(|| Error::NotFound("ffmpeg entry in manifest".into()))?;
    let exe = ffmpeg_path(root, f);
    if exe.is_file() {
        return Ok(exe);
    }
    let name = f.url.rsplit('/').next().unwrap_or("ffmpeg.zip");
    let dest = root.join("downloads").join(format!("ffmpeg-{}-{name}", f.build));
    fetch_with(std::slice::from_ref(&f.url), &dest, &f.sha256, opts, &mut |pr| progress(name, pr))?;
    if opts.is_cancelled() {
        return Err(Error::Cancelled);
    }
    extract_zip_member(&dest, &f.member, &exe)?;
    Ok(exe)
}

/// The installed ffmpeg: the pinned build if present, else the last (by name) other build under
/// `<root>/ffmpeg` (a user-placed binary lands here too).
pub fn installed_ffmpeg(root: &Path, f: &FfmpegManifest) -> Option<PathBuf> {
    let pinned = ffmpeg_path(root, f);
    if pinned.is_file() {
        return Some(pinned);
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join("ffmpeg")).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.into_iter().rev().map(|d| d.join(f.exe_name())).find(|p| p.is_file())
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

    fn make_tgz(path: &Path, files: &[(&str, &[u8])]) {
        let gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (n, c) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(c.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, n, *c).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn tgz_member_extraction() {
        let d = tmp("tgz");
        let t = d.join("p.tgz");
        make_tgz(&t, &[("LICENSE", b"lic"), ("bin/pdfium.dll", b"MZ-dll"), ("lib/pdfium.dll.lib", b"nope")]);
        let out = d.join("x").join("pdfium").join("7881").join("pdfium.dll");
        extract_tgz_member(&t, "bin/pdfium.dll", &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"MZ-dll");
        assert!(!out.with_extension("tmp").exists());
        // overwrite is fine, a missing member is an error and writes nothing
        extract_tgz_member(&t, "bin/pdfium.dll", &out).unwrap();
        let other = d.join("other.dll");
        assert!(matches!(extract_tgz_member(&t, "bin/missing.dll", &other), Err(Error::NotFound(_))));
        assert!(!other.exists());
        assert!(extract_tgz_member(&d.join("nope.tgz"), "bin/pdfium.dll", &other).is_err());
    }

    #[test]
    fn pdfium_path_is_versioned() {
        let m = Manifest::embedded();
        let p = pdfium_path(Path::new("R"), &m.pdfium);
        assert_eq!(p, Path::new("R").join("pdfium").join("7881").join("pdfium.dll"));
    }

    #[test]
    fn zip_member_extraction_and_ffmpeg_lookup() {
        let d = tmp("ffz");
        let z = d.join("f.zip");
        make_zip(&z, &[("ffmpeg-7/LICENSE", b"lic"), ("ffmpeg-7/bin/ffmpeg.exe", b"MZ-ff"), ("ffmpeg-7/bin/ffplay.exe", b"no")]);
        let f = FfmpegManifest {
            build: "7.0".into(),
            url: "https://x/f.zip".into(),
            sha256: "s".into(),
            size: 1,
            member: "ffmpeg-7/bin/ffmpeg.exe".into(),
        };
        let exe = ffmpeg_path(&d, &f);
        assert_eq!(exe, d.join("ffmpeg").join("7.0").join("ffmpeg.exe"));
        assert!(installed_ffmpeg(&d, &f).is_none());
        extract_zip_member(&z, &f.member, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"MZ-ff");
        assert!(!exe.with_extension("tmp").exists() && !exe.with_file_name("ffplay.exe").exists());
        assert_eq!(installed_ffmpeg(&d, &f).unwrap(), exe);
        // an older install is found when the pinned one is absent
        let mut newer = f.clone();
        newer.build = "7.1".into();
        assert_eq!(installed_ffmpeg(&d, &newer).unwrap(), exe);
        assert!(matches!(extract_zip_member(&z, "nope/ffmpeg.exe", &d.join("o.exe")), Err(Error::NotFound(_))));
        assert!(!d.join("o.exe").exists());
    }

    #[test]
    fn ffmpeg_install_needs_a_pinned_hash() {
        let d = tmp("ffi");
        let mut m = Manifest::embedded();
        let opts = FetchOptions::default();
        assert!(
            matches!(install_ffmpeg_with(&d, &m, &opts, &mut |_, _| {}), Err(Error::UnpinnedHash(_))),
            "TODO-pin-in-CI must not download"
        );
        m.ffmpeg = None;
        assert!(matches!(install_ffmpeg_with(&d, &m, &opts, &mut |_, _| {}), Err(Error::NotFound(_))));
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
