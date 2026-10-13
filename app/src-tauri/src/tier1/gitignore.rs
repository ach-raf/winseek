//! Minimal `.gitignore` support for the opt-in source-code scope.
//!
//! Handles comments, `!` negation, trailing `/` (directories only), anchoring (a `/` at the start
//! or middle), `*` `?` `[...]` and `**`. Matching is case-insensitive (Windows). Every
//! `.gitignore` in the folder chain from an include root down to the file is honoured; a deeper
//! file's rules override a shallower one's, and an ignored directory ignores everything below it.
//! Not implemented: `.git/info/exclude`, the global excludes file, `.gitignore` above the include root.

use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// `**`: any number of path segments (at least one when last).
    Any,
    Glob(Vec<char>),
}

#[derive(Debug, Clone)]
struct Pattern {
    segs: Vec<Seg>,
    negate: bool,
    dir_only: bool,
}

/// The rules of one `.gitignore` file.
#[derive(Debug, Clone, Default)]
pub struct Gitignore {
    patterns: Vec<Pattern>,
}

fn glob_match(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        let step = match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => class_match(&p[pi..], t[ti]),
            Some('\\') if pi + 1 < p.len() => (p[pi + 1] == t[ti]).then_some(2),
            Some(&c) => (c == t[ti]).then_some(1),
            None => None,
        };
        match step {
            Some(n) => {
                pi += n;
                ti += 1;
            }
            None => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Match `c` against the `[...]` class at the start of `p`; returns the class length when it matches.
fn class_match(p: &[char], c: char) -> Option<usize> {
    let mut i = 1;
    let neg = matches!(p.get(i), Some('!') | Some('^'));
    if neg {
        i += 1;
    }
    let first = i;
    let mut hit = false;
    while i < p.len() && (p[i] != ']' || i == first) {
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            hit |= p[i] <= c && c <= p[i + 2];
            i += 3;
        } else {
            hit |= p[i] == c;
            i += 1;
        }
    }
    if i >= p.len() {
        // unterminated class: treat '[' literally
        return (c == '[').then_some(1);
    }
    (hit != neg).then_some(i + 1)
}

fn match_segs(p: &[Seg], s: &[String]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((Seg::Any, rest)) => {
            if rest.is_empty() {
                return !s.is_empty();
            }
            (0..=s.len()).any(|k| match_segs(rest, &s[k..]))
        }
        Some((Seg::Glob(g), rest)) => {
            let Some((first, tail)) = s.split_first() else { return false };
            let t: Vec<char> = first.chars().collect();
            glob_match(g, &t) && match_segs(rest, tail)
        }
    }
}

impl Gitignore {
    pub fn parse(text: &str) -> Gitignore {
        let mut patterns = Vec::new();
        for raw in text.lines() {
            let mut line = raw.trim_end_matches(['\r', ' ', '\t']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let negate = match line.strip_prefix('!') {
                Some(rest) => {
                    line = rest;
                    true
                }
                None => false,
            };
            if line.starts_with("\\#") || line.starts_with("\\!") {
                line = &line[1..];
            }
            let dir_only = line.ends_with('/');
            let line = line.trim_end_matches('/');
            if line.is_empty() {
                continue;
            }
            let anchored = line.contains('/');
            let line = line.trim_start_matches('/');
            let mut segs: Vec<Seg> = Vec::new();
            if !anchored {
                segs.push(Seg::Any);
            }
            for part in line.split('/').filter(|p| !p.is_empty()) {
                if part == "**" {
                    if segs.last() != Some(&Seg::Any) {
                        segs.push(Seg::Any);
                    }
                } else {
                    segs.push(Seg::Glob(part.to_lowercase().chars().collect()));
                }
            }
            if segs.is_empty() {
                continue;
            }
            patterns.push(Pattern { segs, negate, dir_only });
        }
        Gitignore { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Verdict for `rel` (lower-cased segments relative to this file's folder): `Some(true)` =
    /// ignored, `Some(false)` = re-included by a `!` rule, `None` = no rule matches.
    fn verdict(&self, rel: &[String], is_dir: bool) -> Option<bool> {
        self.patterns.iter().rev().find(|p| (is_dir || !p.dir_only) && match_segs(&p.segs, rel)).map(|p| !p.negate)
    }
}

/// One `.gitignore` plus where it lives: the (lower-cased) path segments of its folder.
#[derive(Debug, Clone)]
struct Level {
    dir: Vec<String>,
    gi: Arc<Gitignore>,
}

/// The `.gitignore` files from an include root down to some folder.
#[derive(Debug, Clone, Default)]
pub struct Chain {
    levels: Vec<Level>,
}

/// Lower-cased path segments (drive included), separators `\` or `/`.
pub fn segments(path: &str) -> Vec<String> {
    path.split(['\\', '/']).filter(|s| !s.is_empty()).map(str::to_lowercase).collect()
}

impl Chain {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// This chain extended with the `.gitignore` found in `dir` (if any).
    pub fn descend(&self, dir: &Path) -> Chain {
        let Ok(text) = std::fs::read(dir.join(".gitignore")) else { return self.clone() };
        let gi = Gitignore::parse(&String::from_utf8_lossy(&text));
        if gi.is_empty() {
            return self.clone();
        }
        let mut c = self.clone();
        c.levels.push(Level { dir: segments(&dir.to_string_lossy()), gi: Arc::new(gi) });
        c
    }

    /// True when `path` (a file, or a folder if `is_dir`) is ignored by the chain.
    pub fn ignored(&self, path: &str, is_dir: bool) -> bool {
        if self.levels.is_empty() {
            return false;
        }
        let segs = segments(path);
        let first = self.levels.iter().map(|l| l.dir.len()).min().unwrap_or(0);
        for i in (first + 1)..=segs.len() {
            let dir_i = i < segs.len() || is_dir;
            let mut verdict = None;
            for l in &self.levels {
                if l.dir.len() < i && segs.starts_with(&l.dir) {
                    if let Some(v) = l.gi.verdict(&segs[l.dir.len()..i], dir_i) {
                        verdict = Some(v);
                    }
                }
            }
            if verdict == Some(true) {
                return true;
            }
        }
        false
    }

    /// Build the chain for `path` from scratch: every folder from its include `root` down to its
    /// parent is checked for a `.gitignore` (used for single-file change events).
    pub fn for_path(roots: &[String], path: &str) -> Chain {
        let segs = segments(path);
        let best = roots
            .iter()
            .map(|r| (segments(r), r))
            .filter(|(rs, _)| segs.len() > rs.len() && segs[..rs.len()] == rs[..])
            .max_by_key(|(rs, _)| rs.len());
        let Some((rs, root)) = best else { return Chain::default() };
        // Walk the original-case path so the filesystem lookups are exact.
        let parts: Vec<&str> = path.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
        let mut dir = std::path::PathBuf::from(root.trim_end_matches(['\\', '/']));
        if rs.len() == 1 {
            dir.push("\\");
        }
        let mut chain = Chain::default().descend(&dir);
        for part in &parts[rs.len()..parts.len() - 1] {
            dir.push(part);
            chain = chain.descend(&dir);
        }
        chain
    }
}

/// Is `path` ignored by the `.gitignore` files between its include root and its folder?
pub fn is_ignored(roots: &[String], path: &str) -> bool {
    Chain::for_path(roots, path).ignored(path, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn lvl(dir: &str, rules: &str) -> Level {
        Level { dir: segments(dir), gi: Arc::new(Gitignore::parse(rules)) }
    }

    fn ign(rules: &str, path: &str, is_dir: bool) -> bool {
        // rules live in C:\p
        let c = Chain { levels: vec![lvl("C:/p", rules)] };
        c.ignored(&format!(r"C:\p\{path}"), is_dir)
    }

    #[test]
    fn basics_comments_and_blank_lines() {
        let r = "# comment\n\n*.log\nsecret.txt\n";
        assert!(ign(r, "a.log", false));
        assert!(ign(r, r"deep\er\a.LOG", false));
        assert!(ign(r, r"x\secret.txt", false));
        assert!(!ign(r, "a.txt", false));
        assert!(!ign(r, "log", false));
        assert!(!ign("", "a.log", false));
    }

    #[test]
    fn directory_only_and_ancestors() {
        let r = "build/\n";
        assert!(ign(r, "build", true));
        assert!(!ign(r, "build", false), "a file called build is not a directory");
        assert!(ign(r, r"build\out.o", false));
        assert!(ign(r, r"sub\build\deep\x.rs", false));
        assert!(!ign(r, r"buildx\x.rs", false));
    }

    #[test]
    fn anchoring() {
        let r = "/top.rs\ndocs/gen\n";
        assert!(ign(r, "top.rs", false));
        assert!(!ign(r, r"sub\top.rs", false), "leading slash anchors");
        assert!(ign(r, r"docs\gen", false));
        assert!(!ign(r, r"x\docs\gen", false), "a slash in the middle anchors");
    }

    #[test]
    fn negation_and_order() {
        let r = "*.rs\n!keep.rs\n";
        assert!(ign(r, "a.rs", false));
        assert!(!ign(r, "keep.rs", false));
        assert!(!ign(r, r"src\keep.rs", false));
        // last match wins
        let r = "!keep.rs\n*.rs\n";
        assert!(ign(r, "keep.rs", false));
        // cannot re-include below an ignored directory
        let r = "out/\n!out/keep.txt\n";
        assert!(ign(r, r"out\keep.txt", false));
    }

    #[test]
    fn wildcards_classes_and_double_star() {
        assert!(ign("a?c.txt", "abc.txt", false));
        assert!(!ign("a?c.txt", "ac.txt", false));
        assert!(ign("file[0-9].txt", "file7.txt", false));
        assert!(!ign("file[0-9].txt", "filex.txt", false));
        assert!(ign("file[!0-9].txt", "filex.txt", false));
        assert!(ign("**/gen", r"a\b\gen", false));
        assert!(ign("**/gen", "gen", false));
        assert!(ign("a/**/z.txt", r"a\z.txt", false));
        assert!(ign("a/**/z.txt", r"a\b\c\z.txt", false));
        assert!(!ign("a/**/z.txt", r"b\z.txt", false));
        assert!(ign("logs/**", r"logs\a.txt", false));
        assert!(!ign("logs/**", "logs", true), "trailing /** keeps the folder itself");
        assert!(ign("*.min.*", "app.min.js", false));
        assert!(ign(r"\#hash", "#hash", false));
        assert!(ign(r"\!bang", "!bang", false));
    }

    #[test]
    fn deeper_gitignore_overrides_shallower() {
        let root = lvl("C:/p", "*.gen\n");
        let sub = lvl("C:/p/sub", "!keep.gen\n/only-here.txt\n");
        let c = Chain { levels: vec![root, sub] };
        assert!(c.ignored(r"C:\p\x.gen", false));
        assert!(!c.ignored(r"C:\p\sub\keep.gen", false));
        assert!(c.ignored(r"C:\p\sub\other.gen", false));
        assert!(c.ignored(r"C:\p\sub\only-here.txt", false));
        assert!(!c.ignored(r"C:\p\only-here.txt", false), "sub rules do not apply outside sub");
        assert!(!c.ignored(r"C:\p\other\only-here.txt", false));
    }

    #[test]
    fn chain_from_disk() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        fs::create_dir_all(r.join("sub").join("deep")).unwrap();
        fs::write(r.join(".gitignore"), "*.tmp\ngenerated/\n").unwrap();
        fs::write(r.join("sub").join(".gitignore"), "!keep.tmp\nlocal.rs\n").unwrap();
        let roots = vec![r.to_string_lossy().into_owned()];
        let p = |s: &str| r.join(s).to_string_lossy().into_owned();
        assert!(is_ignored(&roots, &p("a.tmp")));
        assert!(!is_ignored(&roots, &p("a.rs")));
        assert!(is_ignored(&roots, &p(r"generated\x.rs")));
        assert!(!is_ignored(&roots, &p(r"sub\keep.tmp")));
        assert!(is_ignored(&roots, &p(r"sub\deep\local.rs")));
        assert!(!is_ignored(&roots, &p(r"local.rs")));
        // outside every root: not our business
        assert!(!is_ignored(&roots, r"Z:\elsewhere\a.tmp"));

        // descend() builds the same chain incrementally (what the crawler does)
        let c = Chain::default().descend(r).descend(&r.join("sub"));
        assert!(c.ignored(&p(r"sub\deep\local.rs"), false));
        assert!(!c.ignored(&p(r"sub\keep.tmp"), false));
        assert!(Chain::default().descend(&r.join("deep-none")).is_empty());
    }
}
