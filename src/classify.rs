//! Classification: which classes a change set selects. Fails open: any doubt
//! selects every class.

use crate::config::Config;
use crate::graph::{self, Graph};
use crate::repo::{basename, dirname, Repo};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct Verdict {
    pub run_all: bool,
    pub reason: Option<String>,
    pub classes: BTreeMap<String, bool>,
    pub unknown_paths: Vec<String>,
    pub inputs_hash: Option<String>,
    /// Human-readable notes (unresolved classes); stderr only.
    pub notes: Vec<String>,
    /// Emit a GitHub `::warning::` (internal failures).
    pub warn: bool,
    /// class -> unresolved inputs as {code, input} (REQ-021); always in the JSON.
    pub unresolved: BTreeMap<String, Vec<serde_json::Value>>,
    /// class -> why it was selected: {path, claim, via} per changed path, or
    /// {unresolved} (REQ-020; printed with --explain).
    pub explain: BTreeMap<String, Vec<serde_json::Value>>,
}

impl Verdict {
    pub fn all(cfg: &Config, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Verdict {
            run_all: true,
            warn: reason.starts_with("internal:"),
            reason: Some(reason),
            classes: cfg.classes.iter().map(|c| (c.name.clone(), true)).collect(),
            unknown_paths: vec![],
            inputs_hash: None,
            notes: vec![],
            unresolved: BTreeMap::new(),
            explain: BTreeMap::new(),
        }
    }
}

pub fn is_run_all(cfg: &Config, p: &str) -> bool {
    run_all_except(cfg, p, None, None)
}

/// Whether `p` is a run-all path for a reason other than the run-all prefix
/// `skip_prefix` or basename `skip_base` (REQ-030 narrows only `.github/`,
/// REQ-027 only `Cargo.lock`).
fn run_all_except(
    cfg: &Config,
    p: &str,
    skip_prefix: Option<&str>,
    skip_base: Option<&str>,
) -> bool {
    let base = basename(p);
    cfg.run_all_prefixes
        .iter()
        .filter(|x| Some(x.as_str()) != skip_prefix)
        .any(|x| p.starts_with(x.as_str()))
        || cfg.run_all_files.iter().any(|x| x == p)
        || cfg
            .run_all_basenames
            .iter()
            .filter(|x| Some(x.as_str()) != skip_base)
            .any(|x| x == base)
        || cfg.run_all_suffixes.iter().any(|x| p.ends_with(x.as_str()))
        || p.split('/')
            .rev()
            .skip(1)
            .any(|c| cfg.run_all_dirs.iter().any(|d| d == c))
}

pub fn is_inert(cfg: &Config, p: &str) -> bool {
    cfg.inert_prefixes.iter().any(|x| p.starts_with(x.as_str()))
        || cfg.inert_files.iter().any(|x| x == p)
}

fn docs_candidate(p: &str) -> bool {
    let b = basename(p);
    p.ends_with(".md") || b == "LICENSE" || b.starts_with("LICENSE-")
}

/// Which docs basenames some tracked non-markdown file names. One pass over
/// the tree for all candidates (the reference's per-file `git grep` took 50 s
/// for 1,200 .md files). A file that cannot be read counts as naming them all.
pub fn docs_named(
    repo: &Repo,
    cfg: &Config,
    candidates: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let mut named = BTreeSet::new();
    if candidates.is_empty() {
        return Ok(named);
    }
    let pats: Vec<&String> = candidates.iter().collect();
    let ac = aho_corasick::AhoCorasick::new(&pats).map_err(|e| format!("docs index: {e}"))?;
    for f in repo.tracked()? {
        if f.ends_with(".md") || f == cfg.path || cfg.docs_namer_exclude.contains(&f) {
            continue;
        }
        match std::fs::read(repo.path(&f)) {
            Ok(bytes) => {
                for m in ac.find_overlapping_iter(&bytes) {
                    named.insert(pats[m.pattern().as_usize()].clone());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // tracked but deleted in the worktree
            Err(_) => return Ok(candidates.clone()),
        }
        if named.len() == candidates.len() {
            break;
        }
    }
    Ok(named)
}

pub fn inputs_hash(cfg: &Config, g: &Graph) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cfg.digest.as_bytes());
    for (c, cg) in g {
        h.update(b"\0class\0");
        h.update(c.as_bytes());
        for (claim, src) in &cg.claims {
            h.update(b"\0");
            h.update(claim.as_bytes());
            h.update(b"=");
            h.update(src.as_bytes());
        }
        for u in &cg.unresolved {
            h.update(b"\0unresolved\0");
            h.update(u.to_string().as_bytes());
        }
    }
    format!("{:x}", h.finalize())
}

/// The base and head commits of a change set, when known (git mode and the
/// equivalence replay); leaf-workflow scoping (REQ-030) needs both versions.
#[derive(Debug, Clone, Copy)]
pub struct Revs<'a> {
    pub base: &'a str,
    pub head: &'a str,
}

/// REQ-030: `None` when `p` is not a candidate (scoping off, not a direct
/// child of `.github/workflows/`, or run-all for another reason); otherwise
/// the selected classes (class -> jobs), or why it still selects everything.
fn leaf_scope(
    repo: &Repo,
    cfg: &Config,
    p: &str,
    revs: Option<Revs>,
) -> Option<Result<BTreeMap<String, BTreeSet<String>>, String>> {
    if !cfg.workflows.scope_leaf_edits {
        return None;
    }
    let name = p.strip_prefix(".github/workflows/")?;
    if name.contains('/') || !(name.ends_with(".yml") || name.ends_with(".yaml")) {
        return None;
    }
    if run_all_except(cfg, p, Some(".github/"), None) {
        return None;
    }
    let Some(r) = revs else {
        return Some(Err(
            "leaf-workflow scoping needs the base version (git mode)".into(),
        ));
    };
    Some(
        crate::gitrepo::blob_pair(&repo.root, r.base, r.head, p).and_then(|pair| match pair {
            (Some(b), Some(h)) => crate::workflows::leaf_scope(cfg, p, &b, &h),
            (None, _) => Err(format!("{p} is added")),
            (_, None) => Err(format!("{p} is deleted")),
        }),
    )
}

/// REQ-027: `None` when `p` is not a nested `Cargo.lock` (the root one, or a
/// path that is run-all for another reason); `Ok` when a class's Cargo walk
/// claims it as its workspace's lockfile; otherwise why it still selects
/// everything.
fn nested_lock(repo: &Repo, cfg: &Config, p: &str, g: &Graph) -> Option<Result<(), String>> {
    if basename(p) != "Cargo.lock" || p == "Cargo.lock" {
        return None;
    }
    if run_all_except(cfg, p, None, Some("Cargo.lock")) {
        return None;
    }
    if !g
        .values()
        .any(|cg| cg.claims.get(p).is_some_and(|s| *s == "cargo-lock"))
    {
        return Some(Err(
            "no class's Cargo walk reaches the workspace that owns it".into(),
        ));
    }
    match bazel_names(repo, p) {
        Ok(None) => Some(Ok(())),
        Ok(Some(f)) => Some(Err(format!(
            "{f} names it (a crate_universe lockfile feeds Bazel targets sieve does not trace to it, DD-009)"
        ))),
        Err(e) => Some(Err(e)),
    }
}

/// The first tracked Bazel file (MODULE.bazel*, WORKSPACE*, BUILD, BUILD.bazel,
/// *.bzl) that names lockfile `p`: by path, by label `//dir:Cargo.lock`, or as
/// `Cargo.lock` from a BUILD file in the same directory.
fn bazel_names(repo: &Repo, p: &str) -> Result<Option<String>, String> {
    let dir = dirname(p);
    let label = format!("//{dir}:Cargo.lock");
    for f in repo.tracked()? {
        let b = basename(&f);
        let build = b == "BUILD" || b == "BUILD.bazel";
        if !(build
            || b.starts_with("MODULE.bazel")
            || b.starts_with("WORKSPACE")
            || b.ends_with(".bzl"))
        {
            continue;
        }
        let text = match std::fs::read(repo.path(&f)) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read {f}: {e}")),
        };
        if text.contains(p)
            || text.contains(&label)
            || (build && dirname(&f) == dir && text.contains("Cargo.lock"))
        {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

/// Classify an explicit change set against a derived graph.
pub fn classify(
    repo: &Repo,
    cfg: &Config,
    changed: &[String],
    g: &Graph,
) -> Result<Verdict, String> {
    classify_revs(repo, cfg, changed, g, None)
}

/// [`classify`] with the change set's commits, when known.
pub fn classify_revs(
    repo: &Repo,
    cfg: &Config,
    changed: &[String],
    g: &Graph,
    revs: Option<Revs>,
) -> Result<Verdict, String> {
    let changed: Vec<String> = changed
        .iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    if changed.is_empty() {
        return Ok(Verdict::all(cfg, "no changed paths"));
    }
    let mut v = Verdict {
        run_all: false,
        reason: None,
        classes: cfg
            .classes
            .iter()
            .map(|c| (c.name.clone(), false))
            .collect(),
        unknown_paths: vec![],
        inputs_hash: Some(inputs_hash(cfg, g)),
        notes: vec![],
        warn: false,
        unresolved: BTreeMap::new(),
        explain: BTreeMap::new(),
    };
    for (c, cg) in g {
        if !cg.unresolved.is_empty() {
            v.unresolved
                .insert(c.clone(), cg.unresolved.iter().map(|u| u.json()).collect());
            v.explain.entry(c.clone()).or_default().extend(
                cg.unresolved
                    .iter()
                    .map(|u| serde_json::json!({"unresolved": u.json()})),
            );
        }
        if let Some(u) = cg.unresolved.first() {
            v.classes.insert(c.clone(), true);
            v.notes
                .push(format!("{c}: unresolved input ({u}); always runs"));
        }
    }
    let candidates: BTreeSet<String> = changed
        .iter()
        .filter(|p| docs_candidate(p))
        .map(|p| basename(p).to_string())
        .collect();
    let named = docs_named(repo, cfg, &candidates)?;
    let mut run_all: Option<String> = None;
    for p in &changed {
        // A control character could forge a workflow command in the log.
        let malformed = p.starts_with('/')
            || p.chars().any(char::is_control)
            || p.split('/').any(|c| c == ".." || c == "." || c.is_empty());
        if malformed {
            v.unknown_paths.push(p.clone());
            run_all.get_or_insert_with(|| {
                format!("run-all path: {p} is not a repository-relative path")
            });
            continue;
        }
        if is_run_all(cfg, p) {
            if let Some(r) = nested_lock(repo, cfg, p, g) {
                match r {
                    Ok(()) => {
                        // every class that claims it, the Cargo walks first
                        for (c, cg) in g.iter().filter(|(_, cg)| cg.claims_path(p)) {
                            if let Some((claim, via)) = cg.claim_of(p) {
                                v.explain.entry(c.clone()).or_default().push(
                                    serde_json::json!({"path": p, "claim": claim, "via": via}),
                                );
                            }
                            v.classes.insert(c.clone(), true);
                        }
                    }
                    Err(why) => {
                        run_all.get_or_insert_with(|| {
                            format!("run-all path: {p} is a nested Cargo.lock that is not scoped: {why}")
                        });
                    }
                }
                continue;
            }
            match leaf_scope(repo, cfg, p, revs) {
                Some(Ok(sel)) => {
                    for (c, jobs) in sel {
                        v.explain.entry(c.clone()).or_default().push(
                            serde_json::json!({"path": p, "claim": "leaf workflow", "via": jobs}),
                        );
                        v.classes.insert(c, true);
                    }
                    for (c, cg) in g.iter().filter(|(_, cg)| cg.claims_path(p)) {
                        if let Some((claim, via)) = cg.claim_of(p) {
                            v.explain
                                .entry(c.clone())
                                .or_default()
                                .push(serde_json::json!({"path": p, "claim": claim, "via": via}));
                        }
                        v.classes.insert(c.clone(), true);
                    }
                }
                Some(Err(why)) => {
                    run_all.get_or_insert_with(|| {
                        format!(
                            "run-all path: {p} is CI and not a scoped leaf-workflow edit: {why}"
                        )
                    });
                }
                None => {
                    run_all.get_or_insert_with(|| {
                        format!("run-all path: {p} is CI, toolchain, build-system or sieve config")
                    });
                }
            }
            continue;
        }
        let hits: Vec<&String> = g
            .iter()
            .filter(|(_, cg)| cg.claims_path(p))
            .map(|(c, _)| c)
            .collect();
        // Docs only if no class claims it: a .md inside a claimed directory can
        // be read through include_dir!, a directory walk or a built name.
        if hits.is_empty() && docs_candidate(p) && !named.contains(basename(p)) {
            continue; // docs only
        }
        for c in &hits {
            if let Some((claim, via)) = g[*c].claim_of(p) {
                v.explain
                    .entry((*c).clone())
                    .or_default()
                    .push(serde_json::json!({"path": p, "claim": claim, "via": via}));
            }
            v.classes.insert((*c).clone(), true);
        }
        if is_inert(cfg, p) {
            if !hits.is_empty() {
                run_all.get_or_insert_with(|| {
                    format!("run-all path: {p} is inert AND an input of {hits:?}")
                });
            }
            continue;
        }
        if hits.is_empty() {
            v.unknown_paths.push(p.clone());
            run_all.get_or_insert_with(|| format!("run-all path: {p} is claimed by no class"));
        }
    }
    if let Some(r) = run_all {
        let unknown = std::mem::take(&mut v.unknown_paths);
        let notes = std::mem::take(&mut v.notes);
        let hash = v.inputs_hash.take();
        let unresolved = std::mem::take(&mut v.unresolved);
        let explain = std::mem::take(&mut v.explain);
        v = Verdict::all(cfg, r);
        v.unknown_paths = unknown;
        v.notes = notes;
        v.inputs_hash = hash;
        v.unresolved = unresolved;
        v.explain = explain;
    }
    Ok(v)
}

/// Derive the graph, then classify; a graph that cannot be derived selects all.
pub fn classify_paths(repo: &Repo, cfg: &Config, changed: &[String]) -> Verdict {
    classify_paths_revs(repo, cfg, changed, None)
}

/// [`classify_paths`] with the change set's commits, when known.
pub fn classify_paths_revs(
    repo: &Repo,
    cfg: &Config,
    changed: &[String],
    revs: Option<Revs>,
) -> Verdict {
    if changed.iter().all(|c| c.trim().is_empty()) {
        return Verdict::all(cfg, "no changed paths");
    }
    match graph::derive(repo, cfg) {
        Ok(g) => classify_revs(repo, cfg, changed, &g, revs)
            .unwrap_or_else(|e| Verdict::all(cfg, format!("internal: {e}"))),
        Err(e) => Verdict::all(
            cfg,
            format!(
                "internal: build graph unreadable: {}: {}",
                e.code, e.message
            ),
        ),
    }
}

/// git mode: classify `base...head` for a pull_request event; anything else
/// (another event, no base, a failed or empty diff) selects everything.
pub fn git_mode(
    root: &std::path::Path,
    cfg: &Config,
    event: Option<&str>,
    base: Option<&str>,
    head: Option<&str>,
) -> Verdict {
    let repo = Repo {
        root: root.to_path_buf(),
        git: true,
    };
    let ev = event.unwrap_or("");
    if ev != "pull_request" {
        return Verdict::all(
            cfg,
            format!(
                "event {} is not pull_request",
                if ev.is_empty() { "<none>" } else { ev }
            ),
        );
    }
    let base = base.unwrap_or("").trim();
    if base.is_empty() {
        return Verdict::all(cfg, "no base sha");
    }
    let head = head.unwrap_or("HEAD");
    // gix reads loose objects and packs through mmap, which WASI lacks: fail
    // open with the reason on stdout (plant's controls read it), instead of
    // failing deep inside gix (REQ-031).
    if cfg!(target_os = "wasi") {
        return Verdict::all(
            cfg,
            "git mode is unsupported in the WASI component (gix reads objects through mmap); pass the changed paths with --paths or --paths-file",
        );
    }
    // A rename is a delete plus an add, and both are classified (gix, in-process).
    match crate::gitrepo::diff_names(root, base, head) {
        Err(e) => Verdict::all(cfg, format!("git diff failed against {base}: {e}")),
        Ok(paths) => {
            if paths.is_empty() {
                Verdict::all(cfg, "empty diff")
            } else {
                classify_paths_revs(&repo, cfg, &paths, Some(Revs { base, head }))
            }
        }
    }
}
