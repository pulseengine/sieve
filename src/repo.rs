//! The repository on disk. With `git = false` sieve reads no git data at all
//! (plant's `--paths-file` and `graph` without .git); with `git = true` it reads
//! the index in-process through gix, never through a `git` process.

use std::path::PathBuf;

pub struct Repo {
    pub root: PathBuf,
    pub git: bool,
}

/// Lexically join `base` (repository-relative) and `p`. None if the result
/// leaves the repository. The repository root is "".
pub fn norm(base: &str, p: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let joined = if p.starts_with('/') {
        p.to_string()
    } else {
        format!("{base}/{p}")
    };
    if p.starts_with('/') {
        return None;
    }
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

pub fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

pub fn basename(p: &str) -> &str {
    p.rsplit_once('/').map(|(_, b)| b).unwrap_or(p)
}

impl Repo {
    pub fn path(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }
    pub fn is_dir(&self, rel: &str) -> bool {
        self.path(rel).is_dir()
    }
    pub fn is_file(&self, rel: &str) -> bool {
        self.path(rel).is_file()
    }
    pub fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }
    pub fn read(&self, rel: &str) -> std::io::Result<String> {
        let b = std::fs::read(self.path(rel))?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    /// An existing directory is claimed as a prefix ("d/"), anything else exactly.
    pub fn claim_for(&self, rel: Option<String>) -> Option<String> {
        let rel = rel?;
        if rel.is_empty() {
            return None;
        }
        Some(if self.is_dir(&rel) {
            format!("{}/", rel.trim_end_matches('/'))
        } else {
            rel
        })
    }

    /// Files under `rel` (repository-relative), skipping .git and target dirs.
    pub fn walk(&self, rel: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![rel.to_string()];
        while let Some(d) = todo.pop() {
            let Ok(rd) = std::fs::read_dir(self.path(&d)) else {
                continue;
            };
            let mut ents: Vec<_> = rd.flatten().collect();
            ents.sort_by_key(|e| e.file_name());
            for e in ents {
                let name = e.file_name().to_string_lossy().into_owned();
                let r = if d.is_empty() {
                    name.clone()
                } else {
                    format!("{d}/{name}")
                };
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if name != ".git" && name != "target" {
                        todo.push(r);
                    }
                } else {
                    out.push(r);
                }
            }
        }
        out.sort();
        out
    }

    /// The repository's files. In git mode the index (an error is an error).
    /// Otherwise the index too when the tree is a git repository (read
    /// in-process, no git call), so untracked residue such as worktrees,
    /// build logs and bytecode is not read; a walk when there is no .git or
    /// its index cannot be read. The walk lists a superset, so the fallback
    /// can only widen what the docs rule counts as naming a file.
    pub fn tracked(&self) -> Result<Vec<String>, String> {
        if self.git {
            return crate::gitrepo::index_paths(&self.root);
        }
        if self.root.join(".git").exists() {
            if let Ok(paths) = crate::gitrepo::index_paths(&self.root) {
                return Ok(paths);
            }
        }
        Ok(self.walk(""))
    }

    /// Glob a pattern of `*`/`?` components against the tree.
    pub fn glob(&self, pattern: &str) -> Vec<String> {
        let mut cur = vec![String::new()];
        for comp in pattern.split('/').filter(|c| !c.is_empty() && *c != ".") {
            let mut next = Vec::new();
            for base in &cur {
                if comp == ".." {
                    if let Some(n) = norm(base, "..") {
                        next.push(n);
                    }
                } else if !comp.contains(['*', '?', '[']) {
                    let p = if base.is_empty() {
                        comp.to_string()
                    } else {
                        format!("{base}/{comp}")
                    };
                    if self.exists(&p) {
                        next.push(p);
                    }
                } else if let Ok(rd) = std::fs::read_dir(self.path(base)) {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if !name.starts_with('.') && wildcard(comp, &name) {
                            next.push(if base.is_empty() {
                                name
                            } else {
                                format!("{base}/{name}")
                            });
                        }
                    }
                }
            }
            cur = next;
        }
        cur.sort();
        cur.dedup();
        cur.retain(|p| !p.is_empty());
        cur
    }
}

/// `*` and `?` wildcard match of one path component.
pub fn wildcard(pat: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_resolves_and_refuses_escape() {
        assert_eq!(norm("ffi/src", "../../os/x.rs").as_deref(), Some("os/x.rs"));
        assert_eq!(norm("ffi", "/../a").as_deref(), None);
        assert_eq!(norm("", "..").as_deref(), None);
        assert_eq!(norm("a", "..").as_deref(), Some(""));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*-thin", "wdg-thin"));
        assert!(!wildcard("*-thin", "wdg-thick"));
        assert!(wildcard("a?c", "abc"));
    }
}
