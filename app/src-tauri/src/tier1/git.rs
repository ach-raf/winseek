//! Git as the ignore oracle (DigUp parity): inside a real repository, git decides which
//! files exist for the code index — not our `.gitignore` parser.
//!
//! [`crate::tier1::gitignore::Chain`] reads the `.gitignore` files between an include root and
//! a file. That misses everything git itself brings: the repo's `.git/info/exclude`, the user's
//! global excludes file, patterns that reach above the include root, and nested repositories
//! with their own rules. So where a repository is, `git ls-files` enumerates what git keeps
//! (tracked + untracked-not-ignored) and that set is the truth; the parser stays as the answer
//! everywhere git can't speak (no git binary, not a repository, git failed).
//!
//! The child git runs read-only and neutralised: `core.fsmonitor=false` and
//! `core.hooksPath=NUL` override whatever the repository's config names (a repo that arrived
//! unzipped can point those at programs), and `GIT_OPTIONAL_LOCKS=0` means an editor's busy
//! index is never waited on or taken. Unlike DigUp we deliberately leave the user's and the
//! system's git config in place — the `-c` flags already neutralise the knobs that can execute
//! something for a read-only command, and keeping the config is what makes the global excludes
//! file count.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// The files git keeps in the repository rooted at some directory, ready for membership tests.
/// Paths are lower-cased (Windows file names compare case-insensitively) and use `/`
/// separators, exactly how git prints them.
pub struct RepoFiles {
    root: String,
    files: HashSet<String>,
}

fn lower_rel(root: &str, path: &str) -> Option<String> {
    let path = path.to_ascii_lowercase();
    let root = root.to_ascii_lowercase();
    let rest = path.strip_prefix(&root)?;
    let rest = rest.trim_start_matches(['\\', '/']);
    if rest.is_empty() {
        None
    } else {
        Some(rest.replace('\\', "/"))
    }
}

impl RepoFiles {
    /// Is `path` (absolute, as the crawler sees it) one of the files git keeps?
    pub fn contains(&self, path: &str) -> bool {
        lower_rel(&self.root, path).is_some_and(|rel| self.files.contains(&rel))
    }
}

/// Run the neutralised read-only git. `None` when git is missing, not a repository there, or
/// it failed — the caller falls back to the `.gitignore` chain.
fn git_output(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=NUL"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// What git keeps in the repository at `dir` (which must hold `.git`): tracked files and
/// untracked files its ignores leave alone.
pub fn list_repo(dir: &Path) -> Option<RepoFiles> {
    let stdout = git_output(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let root = dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_owned();
    let files: HashSet<String> = String::from_utf8_lossy(&stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    Some(RepoFiles { root, files })
}

/// Does git ignore `path`? `Some(true)` / `Some(false)` are git's verdict (exit status —
/// `check-ignore -q` answers without printing), `None` when git can't say. Runs in the file's
/// own directory so git finds the enclosing repository itself: nested repositories, worktrees
/// and submodules all resolve the way git sees them.
pub fn check_ignored(path: &str) -> Option<bool> {
    let dir = Path::new(path).parent()?;
    let out = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=NUL"])
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--", path])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Where the walk stands relative to git: [`RepoCtx::Git`] answers membership from a real
/// `ls-files` listing; anything else falls back to the `.gitignore` chain.
#[derive(Clone)]
pub enum RepoCtx {
    /// No repository seen above or at the walk's directories.
    Outside,
    /// Inside a repository, but git can't enumerate it (missing binary, failure) — the chain
    /// answers, like before this module existed.
    Unavailable,
    /// Inside a repository whose files git listed.
    Git(Arc<RepoFiles>),
}

impl RepoCtx {
    /// The context for a walk root: the nearest ancestor (or the root itself) holding `.git`.
    /// Stops at the drive level and never claims the user profile, mirroring
    /// [`crate::tier1::guard::code_project_root`].
    pub fn for_root(root: &Path) -> RepoCtx {
        let profile = std::env::var("USERPROFILE")
            .unwrap_or_default()
            .trim_end_matches('\\')
            .to_lowercase();
        let mut dir: Option<&Path> = Some(root);
        while let Some(d) = dir {
            if d.components().count() <= 2
                || (!profile.is_empty()
                    && d.to_string_lossy().trim_end_matches('\\').to_lowercase() == profile)
            {
                break;
            }
            if d.join(".git").exists() {
                return list_repo(d).map_or(RepoCtx::Unavailable, |rf| RepoCtx::Git(Arc::new(rf)));
            }
            dir = d.parent();
        }
        RepoCtx::Outside
    }

    /// The context for a directory the walk steps into: a `.git` here starts the directory's
    /// own repository (nested repos, submodules, worktrees), anything else inherits.
    pub fn descend(dir: &Path, parent: &RepoCtx) -> RepoCtx {
        if dir.join(".git").exists() {
            return list_repo(dir).map_or(RepoCtx::Unavailable, |rf| RepoCtx::Git(Arc::new(rf)));
        }
        parent.clone()
    }

    /// Is a code file at `path` out? In a listed repository: git doesn't keep it. Everywhere
    /// else the `.gitignore` chain fallback answers (both branches speak "excluded").
    pub fn excludes(&self, path: &str, chain_fallback: impl FnOnce() -> bool) -> bool {
        match self {
            RepoCtx::Git(rf) => !rf.contains(path),
            _ => chain_fallback(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn git(args: &[&str], dir: &Path) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .expect("git should be runnable for these tests");
        assert!(
            st.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    fn have_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A repo with every ignore source git has: a committed file, an untracked file, a
    /// `.gitignore` hit, a `.git/info/exclude` hit, and a nested repository.
    fn make_repo() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().to_path_buf();
        git(&["init", "-q"], &r);
        fs::write(r.join("kept.rs"), "fn main() {}").unwrap();
        fs::write(r.join(".gitignore"), "*.log\n").unwrap();
        fs::write(r.join("noise.log"), "log").unwrap();
        fs::write(r.join("untracked.py"), "x = 1").unwrap();
        fs::write(r.join(".git").join("info").join("exclude"), "gen/\n").unwrap();
        fs::create_dir_all(r.join("gen")).unwrap();
        fs::write(r.join("gen").join("out.rs"), "fn gen() {}").unwrap();
        git(&["add", "kept.rs"], &r);
        // The nested repo: its own rules, invisible to the outer ls-files.
        let nested = r.join("sub").join("nested");
        fs::create_dir_all(&nested).unwrap();
        git(&["init", "-q"], &nested);
        fs::write(nested.join("inner.py"), "y = 2").unwrap();
        git(&["add", "inner.py"], &nested);
        (d, r)
    }

    #[test]
    fn ls_files_lists_tracked_and_untracked_not_ignored_and_nested_repos_stay_out() {
        if !have_git() {
            eprintln!("skipping: no git binary");
            return;
        }
        let (_d, r) = make_repo();
        let rf = list_repo(&r).expect("a fresh repo must list");
        for kept in ["kept.rs", "untracked.py", ".gitignore"] {
            assert!(rf.contains(&(rf.root.clone() + "\\" + kept)), "{kept}");
        }
        for gone in ["noise.log", r"gen\out.rs", r"sub\nested\inner.py"] {
            assert!(!rf.contains(&(rf.root.clone() + "\\" + gone)), "{gone}");
        }
        // The nested repo answers for itself.
        let inner = list_repo(&r.join("sub").join("nested")).expect("nested repo lists");
        assert!(inner.contains(&(inner.root.clone() + r"\inner.py")));
    }

    #[test]
    fn check_ignore_answers_from_every_ignore_source_and_none_outside_a_repo() {
        if !have_git() {
            eprintln!("skipping: no git binary");
            return;
        }
        let (_d, r) = make_repo();
        let p = |s: &str| r.join(s).to_string_lossy().into_owned();
        assert_eq!(check_ignored(&p("noise.log")), Some(true), ".gitignore");
        assert_eq!(
            check_ignored(&p(r"gen\more.rs")),
            Some(true),
            "info/exclude"
        );
        assert_eq!(check_ignored(&p("kept.rs")), Some(false), "tracked");
        assert_eq!(check_ignored(&p("untracked.py")), Some(false), "untracked");
        // A path that doesn't exist yet still gets git's would-be verdict.
        assert_eq!(check_ignored(&p(r"gen\future.rs")), Some(true));
        assert_eq!(check_ignored(&p("future.rs")), Some(false));
        // Outside any repository git can't say.
        let out = tempfile::tempdir().unwrap();
        fs::write(out.path().join("x.rs"), "z = 3").unwrap();
        assert_eq!(
            check_ignored(&out.path().join("x.rs").to_string_lossy()),
            None,
            "not a repo: fall back to the chain"
        );
    }

    #[test]
    fn repo_ctx_walks_up_from_a_root_and_descends_into_nested_repos() {
        if !have_git() {
            eprintln!("skipping: no git binary");
            return;
        }
        let (_d, r) = make_repo();
        // A root BELOW the repo root still finds it (the include root can be a subfolder).
        let ctx = RepoCtx::for_root(&r.join("sub"));
        let inner = r.join("sub").join("nested").join("inner.py");
        let ps = inner.to_string_lossy().into_owned();
        assert!(
            ctx.excludes(&ps, || true),
            "outer repo does not keep the nested repo's file"
        );
        let kept = r.join("kept.rs").to_string_lossy().into_owned();
        assert!(
            !ctx.excludes(&kept, || false),
            "outer repo keeps its own file"
        );

        // Stepping into the nested repo switches to its listing.
        let ctx2 = RepoCtx::descend(&r.join("sub").join("nested"), &ctx);
        assert!(
            !ctx2.excludes(&ps, || false),
            "nested repo keeps its own file"
        );
        let gone = r
            .join("sub")
            .join("nested")
            .join("nope.rs")
            .to_string_lossy()
            .into_owned();
        assert!(
            ctx2.excludes(&gone, || false),
            "a file the nested repo doesn't know is out even though the chain would allow it"
        );

        // Outside any repo the chain answers.
        let out = tempfile::tempdir().unwrap();
        let ctx3 = RepoCtx::for_root(out.path());
        let any = out.path().join("a.rs").to_string_lossy().into_owned();
        assert!(ctx3.excludes(&any, || true), "chain fallback true");
        assert!(!ctx3.excludes(&any, || false), "chain fallback false");
    }
}
