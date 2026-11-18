//! Derive each class's inputs from the build graph at the checked-out head.
//!
//! A `GraphError` means the graph cannot be derived at all: classification
//! then selects everything. An `unresolved` entry means one class has an
//! input sieve cannot name: that class always runs.

use crate::config::{Class, Config};
use crate::lex::{lex, unescape};
use crate::repo::{basename, dirname, norm, Repo};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use toml::Value;

#[derive(Debug)]
pub struct GraphError {
    /// A stable machine code (`manifest-unparseable`, `bazel-srcs-not-literal`, ...).
    pub code: &'static str,
    pub message: String,
}

impl GraphError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        GraphError {
            code,
            message: message.into(),
        }
    }
}

/// Where an edge came from, as `sieve graph` reports it.
pub type Source = &'static str;

#[derive(Debug, Default, Clone)]
pub struct ClassGraph {
    /// claim -> source. A claim ending in `/` covers a directory.
    pub claims: BTreeMap<String, Source>,
    pub unresolved: Vec<String>,
}

impl ClassGraph {
    fn add(&mut self, claim: String, src: Source) {
        self.claims.entry(claim).or_insert(src);
    }
    pub fn claims_path(&self, p: &str) -> bool {
        claimed(p, self.claims.keys())
    }
}

pub fn claimed<'a>(p: &str, claims: impl IntoIterator<Item = &'a String>) -> bool {
    claims.into_iter().any(|c| {
        if c.ends_with('/') {
            // the bare directory path too: a submodule (gitlink) bump diffs as `dir`
            p.starts_with(c.as_str()) || p == &c[..c.len() - 1]
        } else {
            p == c
        }
    })
}

pub type Graph = BTreeMap<String, ClassGraph>;

/// Test hook: SIEVE_FAULT=toml makes every manifest unparseable, SIEVE_FAULT=panic
/// panics in the walk. Both can only make sieve select MORE (fail open).
fn fault(kind: &str) -> bool {
    std::env::var("SIEVE_FAULT")
        .map(|v| v.split(',').any(|k| k == kind))
        .unwrap_or(false)
}

pub fn derive(repo: &Repo, cfg: &Config) -> Result<Graph, GraphError> {
    if fault("panic") {
        panic!("SIEVE_FAULT=panic");
    }
    let mut g = Graph::new();
    for class in &cfg.classes {
        g.insert(class.name.clone(), derive_class(repo, cfg, class)?);
    }
    Ok(g)
}

fn derive_class(repo: &Repo, cfg: &Config, class: &Class) -> Result<ClassGraph, GraphError> {
    let mut cg = ClassGraph::default();
    let mut w = CargoWalk {
        repo,
        seen: BTreeSet::new(),
    };
    let mut manifests: Vec<String> = class.cargo.clone();
    for pat in &class.cargo_glob {
        let found = repo.glob(pat);
        if found.is_empty() {
            return Err(GraphError::new(
                "glob-matches-nothing",
                format!("{}: {pat} matches nothing", class.name),
            ));
        }
        manifests.extend(found);
    }
    for m in &manifests {
        w.walk(m, true, &mut cg)?;
    }
    for z in &class.zephyr {
        walk_zephyr(repo, cfg, z, &mut cg)?;
    }
    for b in &class.bazel_build {
        for c in bazel_literal_srcs(repo, b)? {
            cg.add(c, "bazel");
        }
    }
    if let Some(s) = &class.bazel_snapshot {
        bazel_snapshot(repo, s, &mut cg)?;
    }
    for h in &class.hand {
        cg.add(h.clone(), "hand");
    }
    follow_symlinks(repo, &mut cg);
    Ok(cg)
}

// ── Cargo ──────────────────────────────────────────────────────────────────

struct CargoWalk<'a> {
    repo: &'a Repo,
    seen: BTreeSet<(String, bool)>,
}

fn load_toml(repo: &Repo, rel: &str) -> Result<Value, GraphError> {
    let text = repo
        .read(rel)
        .map_err(|e| GraphError::new("manifest-unreadable", format!("{rel}: {e}")))?;
    if fault("toml") {
        return Err(GraphError::new(
            "manifest-unparseable",
            format!("{rel}: SIEVE_FAULT=toml"),
        ));
    }
    toml::from_str(&text).map_err(|e| {
        GraphError::new(
            "manifest-unparseable",
            format!("{rel}: {}", e.to_string().lines().next().unwrap_or("")),
        )
    })
}

const DEP_KEYS: &[&str] = &[
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];

/// (is_dev, name, spec) for every dependency table in a manifest.
fn dep_tables(doc: &Value) -> Vec<(bool, &String, &Value)> {
    fn push<'v>(out: &mut Vec<(bool, &'v String, &'v Value)>, t: &'v Value) {
        for k in DEP_KEYS {
            if let Some(tab) = t.get(*k).and_then(Value::as_table) {
                for (n, spec) in tab {
                    out.push((k.starts_with("dev"), n, spec));
                }
            }
        }
    }
    let mut out = Vec::new();
    push(&mut out, doc);
    if let Some(targets) = doc.get("target").and_then(Value::as_table) {
        for t in targets.values() {
            push(&mut out, t);
        }
    }
    if let Some(ws) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
    {
        for (n, spec) in ws {
            out.push((false, n, spec));
        }
    }
    if let Some(patch) = doc.get("patch").and_then(Value::as_table) {
        for t in patch.values() {
            if let Some(t) = t.as_table() {
                for (n, spec) in t {
                    out.push((false, n, spec));
                }
            }
        }
    }
    out
}

/// The `path` of a dependency spec. A path that is not a string is an error:
/// sieve cannot know what the dependency compiles from.
fn dep_path<'v>(
    manifest: &str,
    name: &str,
    spec: &'v Value,
) -> Result<Option<&'v str>, GraphError> {
    match spec.get("path") {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(GraphError::new(
            "manifest-invalid",
            format!("{manifest}: dependency `{name}` has a path that is not a string ({other})"),
        )),
    }
}

impl CargoWalk<'_> {
    fn walk(
        &mut self,
        manifest: &str,
        as_root: bool,
        cg: &mut ClassGraph,
    ) -> Result<(), GraphError> {
        if !self.seen.insert((manifest.to_string(), as_root)) {
            return Ok(());
        }
        let repo = self.repo;
        let doc = load_toml(repo, manifest)?;
        let mdir = dirname(manifest).to_string();
        cg.add(manifest.to_string(), "cargo");
        let pkg = doc.get("package");

        // The enclosing workspace root's manifest shapes this build too.
        let ws_root = enclosing_workspace(repo, &mdir, &doc);
        if let Some(wr) = &ws_root {
            if wr != manifest {
                cg.add(wr.clone(), "cargo");
            }
        }

        let mut rs_roots: Vec<String> = Vec::new();
        let mut explicit: Vec<String> = Vec::new();
        for kind in ["lib", "bin", "test", "bench", "example"] {
            let items: Vec<&Value> = match doc.get(kind) {
                Some(Value::Array(a)) => a.iter().collect(),
                Some(t @ Value::Table(_)) => vec![t],
                _ => vec![],
            };
            for t in items {
                if let Some(p) = t.get("path").and_then(Value::as_str) {
                    if kind == "lib" || as_root {
                        explicit.push(p.to_string());
                    }
                }
            }
        }
        if pkg.is_some() {
            if !mdir.is_empty() {
                // A crate in its own directory is claimed whole.
                cg.add(format!("{mdir}/"), "cargo");
                rs_roots.extend(repo.walk(&mdir).into_iter().filter(|f| f.ends_with(".rs")));
            } else {
                // The repository-root package shares its directory with
                // everything, so its targets are claimed one by one.
                let has_lib_path = doc.get("lib").and_then(|l| l.get("path")).is_some();
                if !has_lib_path && repo.is_file("src/lib.rs") {
                    explicit.push("src/lib.rs".into());
                }
                if as_root {
                    let auto = [
                        ("bin", "src/bin"),
                        ("test", "tests"),
                        ("bench", "benches"),
                        ("example", "examples"),
                    ];
                    for (kind, d) in auto {
                        let off = pkg
                            .and_then(|p| p.get(format!("auto{kind}s")))
                            .and_then(Value::as_bool)
                            == Some(false);
                        if off || !repo.is_dir(d) {
                            continue;
                        }
                        let mut ents: Vec<_> = std::fs::read_dir(repo.path(d))
                            .map(|rd| {
                                rd.flatten()
                                    .map(|e| e.file_name().to_string_lossy().into_owned())
                                    .collect()
                            })
                            .unwrap_or_default();
                        ents.sort();
                        for e in ents {
                            let p = format!("{d}/{e}");
                            if e.ends_with(".rs") {
                                explicit.push(p);
                            } else if repo.is_file(&format!("{p}/main.rs")) {
                                explicit.push(format!("{p}/main.rs"));
                                // a multi-file target: its directory is its source
                                cg.add(format!("{p}/"), "cargo");
                            }
                        }
                    }
                    if repo.is_file("src/main.rs") {
                        explicit.push("src/main.rs".into());
                    }
                }
            }
            let build = match pkg.and_then(|p| p.get("build")) {
                None => Some("build.rs".to_string()),
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Boolean(true)) => Some("build.rs".into()),
                _ => None,
            };
            if let Some(b) = &build {
                if let Some(rel) = norm(&mdir, b) {
                    if repo.is_file(&rel) {
                        cg.add(rel.clone(), "cargo");
                        scan_build_rs(repo, &rel, &mdir, cg);
                        rs_roots.push(rel);
                    }
                }
            }
            if let Some(comp) = pkg
                .and_then(|p| p.get("metadata"))
                .and_then(|m| m.get("component"))
            {
                metadata_component_paths(repo, &mdir, comp, cg);
            }
            for f in ["readme", "license-file"] {
                if let Some(p) = pkg.and_then(|p| p.get(f)).and_then(Value::as_str) {
                    if let Some(c) = repo.claim_for(norm(&mdir, p)) {
                        cg.add(c, "cargo");
                    }
                }
            }
        }
        for p in explicit {
            if let Some(rel) = norm(&mdir, &p) {
                if mdir.is_empty() && p.ends_with("lib.rs") {
                    // the root package's library directory, as the reference does
                    let d = dirname(&rel);
                    if !d.is_empty() {
                        cg.add(format!("{d}/"), "cargo");
                    }
                }
                cg.add(rel.clone(), "cargo");
                rs_roots.push(rel);
            }
        }

        // #[path], include!, generate!, string literals; the module tree.
        let mut seen = BTreeSet::new();
        let mut todo = rs_roots;
        while let Some(f) = todo.pop() {
            if !seen.insert(f.clone()) {
                continue;
            }
            for sub in scan_rs(repo, &f, &mdir, cg) {
                cg.add(sub.clone(), "cargo");
                todo.push(sub);
            }
        }

        // Path dependencies, transitively.
        for (is_dev, name, spec) in dep_tables(&doc) {
            if is_dev && !as_root {
                continue;
            }
            if let Some(p) = dep_path(manifest, name, spec)? {
                self.walk_dep(manifest, &mdir, p, cg)?;
            } else if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
                if let Some(wr) = &ws_root {
                    if wr != manifest {
                        let wdoc = load_toml(repo, wr)?;
                        let wspec = wdoc
                            .get("workspace")
                            .and_then(|w| w.get("dependencies"))
                            .and_then(|d| d.get(name.as_str()));
                        if let Some(ws) = wspec {
                            if let Some(p) = dep_path(wr, name, ws)? {
                                self.walk_dep(wr, dirname(wr), p, cg)?;
                            }
                        }
                    }
                }
            }
        }
        if let Some(members) = doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(Value::as_array)
        {
            for pat in members.iter().filter_map(Value::as_str) {
                let full = if mdir.is_empty() {
                    pat.to_string()
                } else {
                    format!("{mdir}/{pat}")
                };
                for d in repo.glob(&full) {
                    let m = format!("{d}/Cargo.toml");
                    if repo.is_file(&m) {
                        self.walk(&m, as_root, cg)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_dep(
        &mut self,
        manifest: &str,
        mdir: &str,
        p: &str,
        cg: &mut ClassGraph,
    ) -> Result<(), GraphError> {
        let dep_dir = norm(mdir, p).ok_or_else(|| {
            GraphError::new(
                "path-dep-leaves-repo",
                format!("{manifest}: path dependency {p} leaves the repository"),
            )
        })?;
        let m = if dep_dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dep_dir}/Cargo.toml")
        };
        self.walk(&m, false, cg)
    }
}

/// The nearest manifest at or above `mdir` that declares [workspace].
fn enclosing_workspace(repo: &Repo, mdir: &str, doc: &Value) -> Option<String> {
    if doc.get("workspace").is_some() {
        return Some(if mdir.is_empty() {
            "Cargo.toml".into()
        } else {
            format!("{mdir}/Cargo.toml")
        });
    }
    if let Some(p) = doc
        .get("package")
        .and_then(|p| p.get("workspace"))
        .and_then(Value::as_str)
    {
        let d = norm(mdir, p)?;
        return Some(if d.is_empty() {
            "Cargo.toml".into()
        } else {
            format!("{d}/Cargo.toml")
        });
    }
    let mut d = mdir.to_string();
    while !d.is_empty() {
        d = dirname(&d).to_string();
        let m = if d.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{d}/Cargo.toml")
        };
        if let Ok(text) = repo.read(&m) {
            if let Ok(v) = toml::from_str::<Value>(&text) {
                if v.get("workspace").is_some() {
                    return Some(m);
                }
            }
        }
    }
    None
}

/// Every `path` string under [package.metadata.component], relative to the manifest.
fn metadata_component_paths(repo: &Repo, mdir: &str, v: &Value, cg: &mut ClassGraph) {
    match v {
        Value::Table(t) => {
            for (k, x) in t {
                if k == "path" {
                    if let Some(s) = x.as_str() {
                        if let Some(c) = repo.claim_for(norm(mdir, s)) {
                            cg.add(c, "cargo");
                        }
                        continue;
                    }
                }
                metadata_component_paths(repo, mdir, x, cg);
            }
        }
        Value::Array(a) => a
            .iter()
            .for_each(|x| metadata_component_paths(repo, mdir, x, cg)),
        _ => {}
    }
}

// ── Rust source scans ──────────────────────────────────────────────────────

const STR: &str = r#""((?:[^"\\]|\\.)*)""#;

struct Res {
    path_attr: Regex,
    cfg_attr_path: Regex,
    include: Regex,
    include_dir: Regex,
    module: Regex,
    join_chain: Regex,
    join_lit: Regex,
    incbin: Regex,
    literal: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        path_attr: Regex::new(&format!(r"#\s*\[\s*path\s*=\s*{STR}\s*\]")).unwrap(),
        cfg_attr_path: Regex::new(&format!(r"#\s*\[\s*cfg_attr\s*\((?:[^\]]*?)\bpath\s*=\s*{STR}")).unwrap(),
        include: Regex::new(&format!(
            r#"\binclude(?:_str|_bytes)?!\s*\(\s*({STR}|concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*{STR}\s*\)|[^)]*)\)"#
        ))
        .unwrap(),
        include_dir: Regex::new(r"\binclude_dir!\s*\(\s*([^)]*)\)").unwrap(),
        module: Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap(),
        join_chain: Regex::new(r#"(?:\.join\(\s*"(?:[^"\\]|\\.)*"\s*\)\s*)+"#).unwrap(),
        join_lit: Regex::new(&format!(r"\.join\(\s*{STR}\s*\)")).unwrap(),
        incbin: Regex::new(r#"\.incbin\s+(?:\\?"([^"\\]*)\\?"|(\S+))"#).unwrap(),
        literal: Regex::new(STR).unwrap(),
    })
}

/// Claim what one .rs file pulls in from elsewhere; return module files to walk.
fn scan_rs(repo: &Repo, file: &str, mdir: &str, cg: &mut ClassGraph) -> Vec<String> {
    let Ok(text) = repo.read(file) else {
        return vec![];
    };
    let lexed = lex(&text);
    let src = &lexed.code;
    let r = res();
    let fdir = dirname(file);
    let mut out = Vec::new();

    for re in [&r.path_attr, &r.cfg_attr_path] {
        for m in re.captures_iter(src) {
            let lit = unescape(&m[1]);
            if let Some(rel) = norm(fdir, &lit) {
                if rel.ends_with(".rs") && repo.is_file(&rel) {
                    out.push(rel.clone());
                }
                if let Some(c) = repo.claim_for(Some(rel)) {
                    cg.add(c, "path-attr");
                }
            }
        }
    }
    for m in r.include.captures_iter(src) {
        let target = if let Some(l) = m.get(2) {
            norm(fdir, &unescape(l.as_str()))
        } else if let Some(l) = m.get(3) {
            norm(mdir, unescape(l.as_str()).trim_start_matches('/'))
        } else {
            // The full, balanced argument: the regex stops at the first `)`.
            let open = m.get(0).unwrap().start() + m[0].find('(').unwrap();
            let full = matching(src.as_bytes(), open).map(|end| src[open + 1..end].trim());
            if full.is_some_and(is_out_dir_include) {
                continue; // generated by the build script, whose inputs are scanned
            }
            let arg = full.unwrap_or(m[1].trim());
            cg.unresolved.push(format!(
                "{file}: include!({})",
                arg.chars().take(60).collect::<String>()
            ));
            continue;
        };
        if let Some(c) = repo.claim_for(target) {
            cg.add(c, "include");
        }
    }
    for m in r.include_dir.captures_iter(src) {
        let arg = m[1].trim();
        let lit = r.literal.captures(arg).map(|c| unescape(&c[1]));
        match lit {
            Some(l) if !l.replacen("$CARGO_MANIFEST_DIR", "", 1).contains('$') => {
                let l = l.replacen("$CARGO_MANIFEST_DIR", "", 1);
                if let Some(c) = repo.claim_for(norm(mdir, l.trim_start_matches('/'))) {
                    cg.add(c, "include");
                }
            }
            _ => cg.unresolved.push(format!(
                "{file}: include_dir!({})",
                arg.chars().take(60).collect::<String>()
            )),
        }
    }
    scan_generate(repo, file, src, mdir, cg);
    for lit in &lexed.literals {
        let lit = unescape(lit);
        for m in r.incbin.captures_iter(&lit) {
            match m.get(1) {
                Some(p) if !p.as_str().contains(['{', '$']) => {
                    if let Some(c) = repo.claim_for(norm(mdir, p.as_str())) {
                        cg.add(c, "include");
                    }
                }
                _ => cg.unresolved.push(format!("{file}: .incbin {}", &m[0])),
            }
        }
        // A literal naming an existing repository path relative to the
        // package root (cargo's cwd for tests and the build): a runtime read.
        // A leading `/` is tried relative too: format!("{}/../x", env!("CARGO_MANIFEST_DIR")).
        if lit.contains('/') || lit.contains('.') {
            // a leading `{}` or `{name}` placeholder stands for the manifest dir
            let after_placeholder = lit
                .strip_prefix('{')
                .and_then(|s| s.split_once('}'))
                .map(|(_, rest)| rest)
                .unwrap_or(&lit);
            for l in [
                lit.as_str(),
                lit.trim_start_matches('/'),
                after_placeholder.trim_start_matches('/'),
            ] {
                if let Some(rel) = norm(mdir, l) {
                    if !rel.is_empty() && repo.exists(&rel) {
                        if let Some(c) = repo.claim_for(Some(rel)) {
                            cg.add(c, "literal");
                        }
                    }
                }
            }
        }
    }
    for m in r.join_chain.find_iter(src) {
        let parts: Vec<String> = r
            .join_lit
            .captures_iter(m.as_str())
            .map(|c| unescape(&c[1]))
            .collect();
        if let Some(rel) = norm(mdir, &parts.join("/")) {
            if repo.exists(&rel) {
                if let Some(c) = repo.claim_for(Some(rel)) {
                    cg.add(c, "literal");
                }
            }
        }
    }

    // Module files this file loads.
    let stem = basename(file).trim_end_matches(".rs");
    let subdir = if ["lib", "main", "mod"].contains(&stem) || basename(file) == "build.rs" {
        fdir.to_string()
    } else {
        norm(fdir, stem).unwrap_or_default()
    };
    for m in r.module.captures_iter(src) {
        for cand in [
            format!("{}/{}.rs", subdir, &m[1]),
            format!("{}/{}/mod.rs", subdir, &m[1]),
        ] {
            let cand = cand.trim_start_matches('/').to_string();
            if repo.is_file(&cand) {
                out.push(cand);
            }
        }
    }
    out
}

/// Exactly `concat!(env!("OUT_DIR"), "<literal>")` with no `..` in the
/// literal: a file the build script generated. Any other form that mentions
/// OUT_DIR could reach back into the source tree and is unresolved.
fn is_out_dir_include(arg: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"^concat!\s*\(\s*env!\s*\(\s*"OUT_DIR"\s*\)\s*,\s*"([^"\\]*)"\s*,?\s*\)$"#)
            .unwrap()
    });
    re.captures(arg)
        .is_some_and(|c| !c[1].split('/').any(|p| p == ".."))
}

/// A symlink at or inside a claim makes the class read its target too: claim
/// the target when it is inside the repository, and leave the class
/// unresolved when it is not. Repeats until no new claim appears (chains).
fn follow_symlinks(repo: &Repo, cg: &mut ClassGraph) {
    let root = repo
        .root
        .canonicalize()
        .unwrap_or_else(|_| repo.root.clone());
    let mut done: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut found: Vec<String> = Vec::new();
        for claim in cg.claims.keys() {
            let base = claim.trim_end_matches('/');
            // every ancestor component, the claim itself, and (for a directory) its contents
            let mut cands: Vec<String> = Vec::new();
            let mut acc = String::new();
            for comp in base.split('/') {
                acc = if acc.is_empty() {
                    comp.to_string()
                } else {
                    format!("{acc}/{comp}")
                };
                cands.push(acc.clone());
            }
            if claim.ends_with('/') {
                cands.extend(repo.walk(base));
            }
            for c in cands {
                if done.contains(&c) {
                    continue;
                }
                if std::fs::symlink_metadata(repo.path(&c))
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    found.push(c);
                }
            }
        }
        if found.is_empty() {
            return;
        }
        for s in found {
            done.insert(s.clone());
            let target = repo.path(&s).canonicalize().ok().and_then(|t| {
                t.strip_prefix(&root)
                    .ok()
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
            });
            match target {
                Some(rel) if !rel.is_empty() => {
                    if let Some(c) = repo.claim_for(Some(rel)) {
                        cg.add(c, "symlink");
                    }
                }
                _ => cg.unresolved.push(format!(
                    "{s}: symlink points outside the repository or nowhere"
                )),
            }
        }
    }
}

/// `wit_bindgen::generate!` paths, found by brace matching so a nested
/// `with: { .. }` before `path:` does not hide it.
fn scan_generate(repo: &Repo, file: &str, src: &str, mdir: &str, cg: &mut ClassGraph) {
    let b = src.as_bytes();
    let mut from = 0;
    while let Some(off) = src[from..].find("generate!") {
        let start = from + off + "generate!".len();
        from = start;
        let mut i = start;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || !matches!(b[i], b'(' | b'{' | b'[') {
            continue;
        }
        let Some(end) = matching(b, i) else {
            cg.unresolved
                .push(format!("{file}: generate! with unbalanced delimiters"));
            continue;
        };
        let body = &src[i + 1..end];
        let inner = body.trim();
        let mut paths: Vec<String> = Vec::new();
        let mut resolved = true;
        if inner.starts_with('{') {
            let ib = inner.as_bytes();
            let Some(iend) = matching(ib, 0) else {
                cg.unresolved
                    .push(format!("{file}: generate! with unbalanced braces"));
                continue;
            };
            match top_level_value(&inner[1..iend], "path") {
                Some(v) => {
                    let v = v.trim();
                    let lits: Vec<String> = res()
                        .literal
                        .captures_iter(v)
                        .map(|c| unescape(&c[1]))
                        .collect();
                    let literal_only =
                        v.starts_with('"') || (v.starts_with('[') && v.ends_with(']'));
                    if lits.is_empty() || !literal_only {
                        resolved = false;
                    }
                    paths = lits;
                }
                None => paths.push("wit".into()),
            }
        } else {
            // generate!("world") or generate!(): the default wit/ directory.
            paths.push("wit".into());
        }
        if !resolved {
            cg.unresolved
                .push(format!("{file}: generate! path is not a literal"));
            continue;
        }
        for p in paths {
            if let Some(c) = repo.claim_for(norm(mdir, &p)) {
                cg.add(c, "wit");
            }
        }
    }
}

/// Index of the delimiter matching b[open], skipping string literals.
fn matching(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// In `k: v, k2: { .. }, ...` the text of the value of `key` at depth 0.
fn top_level_value<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    let mut item_start = 0;
    let mut items = Vec::new();
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                items.push(&s[item_start..i]);
                item_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    items.push(&s[item_start.min(s.len())..]);
    items.into_iter().find_map(|it| {
        let (k, v) = it.split_once(':')?;
        (k.trim() == key).then_some(v)
    })
}

/// Every string literal in a build script that names a path is an input.
fn scan_build_rs(repo: &Repo, file: &str, mdir: &str, cg: &mut ClassGraph) {
    let Ok(text) = repo.read(file) else { return };
    for lit in lex(&text).literals {
        let lit = unescape(&lit)
            .replace("cargo:rerun-if-changed=", "")
            .replace("cargo::rerun-if-changed=", "");
        if !lit.contains('/') && !lit.contains('.') {
            continue;
        }
        if lit.contains(['{', '}', '$', ' ', '\\'])
            || lit.starts_with('-')
            || lit.starts_with("cargo:")
        {
            continue;
        }
        let Some(rel) = norm(mdir, &lit) else {
            continue;
        };
        if repo.exists(&rel) || lit.contains('/') {
            if let Some(c) = repo.claim_for(Some(rel)) {
                cg.add(c, "build-rs");
            }
        }
    }
}

// ── Zephyr ─────────────────────────────────────────────────────────────────

fn walk_zephyr(
    repo: &Repo,
    cfg: &Config,
    module: &str,
    cg: &mut ClassGraph,
) -> Result<(), GraphError> {
    let text = repo
        .read(module)
        .map_err(|e| GraphError::new("zephyr-module-unreadable", format!("{module}: {e}")))?;
    cg.add(module.to_string(), "zephyr");
    let re = Regex::new(r"(?m)^\s*(?:cmake|kconfig|cmake-ext|settings|dts_root|board_root|soc_root|arch_root|module_ext_root)\s*:\s*(\S+)\s*$").unwrap();
    let entries: Vec<String> = re
        .captures_iter(&text)
        .map(|c| c[1].trim_matches(['"', '\'']).to_string())
        .collect();
    if entries.is_empty() {
        return Err(GraphError::new(
            "zephyr-module-empty",
            format!("{module}: no cmake/kconfig entry"),
        ));
    }
    // module.yml paths are relative to the module root, the directory above zephyr/module.yml.
    let mroot = {
        let d = dirname(module);
        if basename(d) == "zephyr" {
            dirname(d).to_string()
        } else {
            d.to_string()
        }
    };
    let var_re = if cfg.cmake_root_vars.is_empty() {
        None
    } else {
        let alt = cfg
            .cmake_root_vars
            .iter()
            .map(|v| regex::escape(v))
            .collect::<Vec<_>>()
            .join("|");
        Some(Regex::new(&format!(r"\$\{{(?:{alt})\}}/([A-Za-z0-9_./-]+)")).unwrap())
    };
    for e in &entries {
        let rel = norm(&mroot, e);
        if let Some(c) = repo.claim_for(rel.clone()) {
            cg.add(c, "zephyr");
        }
        let Some(rel) = rel else { continue };
        if !repo.is_dir(&rel) {
            continue;
        }
        for f in repo.walk(&rel) {
            let b = basename(&f);
            if b == "CMakeLists.txt" || b.ends_with(".cmake") {
                if let (Some(re), Ok(src)) = (&var_re, repo.read(&f)) {
                    for c in re.captures_iter(&src) {
                        if let Some(cl) = repo.claim_for(norm(&mroot, &c[1])) {
                            cg.add(cl, "zephyr");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Bazel ──────────────────────────────────────────────────────────────────

/// The literal `srcs` of a BUILD file, as repository paths. A glob, a
/// variable, a concatenation or an @external label cannot be proven and is
/// the error `bazel-srcs-not-literal`.
pub fn bazel_literal_srcs(repo: &Repo, build: &str) -> Result<Vec<String>, GraphError> {
    let text = repo
        .read(build)
        .map_err(|e| GraphError::new("bazel-build-unreadable", format!("{build}: {e}")))?;
    let code: String = text
        .lines()
        .map(strip_hash_comment)
        .collect::<Vec<_>>()
        .join("\n");
    let pkg = dirname(build).to_string();
    let re = Regex::new(r"\bsrcs\s*=\s*").unwrap();
    let b = code.as_bytes();
    let mut out = Vec::new();
    for m in re.find_iter(&code) {
        let i = m.end();
        let bad =
            |why: &str| GraphError::new("bazel-srcs-not-literal", format!("{build}: srcs {why}"));
        if i >= b.len() || b[i] != b'[' {
            return Err(bad("is not a literal list"));
        }
        let end = matching(b, i).ok_or_else(|| bad("has an unbalanced list"))?;
        let after = code[end + 1..].trim_start();
        if !(after.starts_with(',') || after.starts_with(')')) {
            return Err(bad("is concatenated with a non-literal"));
        }
        let inner = &code[i + 1..end];
        let lit_re = res().literal.clone();
        let rest = lit_re.replace_all(inner, "");
        if rest.chars().any(|c| !(c.is_whitespace() || c == ',')) {
            return Err(bad("holds a non-literal element"));
        }
        for c in lit_re.captures_iter(inner) {
            let label = unescape(&c[1]);
            if label.starts_with('@') {
                return Err(bad(&format!("names the external label {label}")));
            }
            let path = if let Some(l) = label.strip_prefix("//") {
                let (p, n) = l.split_once(':').unwrap_or((l, basename(l)));
                if p.is_empty() {
                    n.to_string()
                } else {
                    format!("{p}/{n}")
                }
            } else {
                let n = label.trim_start_matches(':');
                if pkg.is_empty() {
                    n.to_string()
                } else {
                    format!("{pkg}/{n}")
                }
            };
            out.push(path);
        }
    }
    Ok(out)
}

fn strip_hash_comment(l: &str) -> &str {
    let mut in_str = false;
    for (i, c) in l.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &l[..i],
            _ => {}
        }
    }
    l
}

/// A committed `bazel query` snapshot: its files claim their package
/// directories (so a new file in a glob is covered). A stale fingerprint
/// makes the class unresolved.
fn bazel_snapshot(
    repo: &Repo,
    s: &crate::config::BazelSnapshot,
    cg: &mut ClassGraph,
) -> Result<(), GraphError> {
    let text = repo
        .read(&s.file)
        .map_err(|e| GraphError::new("bazel-snapshot-unreadable", format!("{}: {e}", s.file)))?;
    let snap: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| GraphError::new("bazel-snapshot-unreadable", format!("{}: {e}", s.file)))?;
    let files = snap["gates"][&s.key]["files"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            GraphError::new(
                "bazel-snapshot-unreadable",
                format!("{}: no files for {}", s.file, s.key),
            )
        })?;
    for f in files.iter().filter_map(|f| f.as_str()) {
        let d = dirname(f);
        cg.add(
            if d.is_empty() {
                f.to_string()
            } else {
                format!("{d}/")
            },
            "bazel",
        );
    }
    let fp_files: Vec<String> = snap["fingerprint_files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if bazel_fingerprint(repo, &fp_files) != snap["fingerprint"].as_str().unwrap_or("") {
        cg.unresolved.push(format!(
            "{}: fingerprint is stale (a BUILD/.bzl/MODULE file changed)",
            s.file
        ));
    }
    Ok(())
}

pub fn bazel_fingerprint(repo: &Repo, files: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted = files.to_vec();
    sorted.sort();
    let mut h = Sha256::new();
    for f in sorted {
        h.update(f.as_bytes());
        h.update(b"\0");
        match std::fs::read(repo.path(&f)) {
            Ok(b) => h.update(&b),
            Err(_) => h.update(b"<missing>"),
        }
        h.update(b"\0");
    }
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> Repo {
        let root = std::env::temp_dir().join(format!("sieve-graph-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (p, body) in files {
            let fp = root.join(p);
            std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
            std::fs::write(fp, body).unwrap();
        }
        Repo { root, git: false }
    }

    #[test]
    fn generate_without_path_claims_the_default_wit_dir() {
        let repo = tree(
            "gen-default",
            &[
                ("c/src/lib.rs", "wit_bindgen::generate!({ world: \"w\" });"),
                ("c/wit/w.wit", "x"),
            ],
        );
        let mut cg = ClassGraph::default();
        scan_rs(&repo, "c/src/lib.rs", "c", &mut cg);
        assert!(cg.claims.contains_key("c/wit/"), "{:?}", cg.claims);
        assert_eq!(cg.claims["c/wit/"], "wit");
    }

    #[test]
    fn generate_with_a_non_literal_path_is_unresolved() {
        let repo = tree(
            "gen-var",
            &[(
                "c/src/lib.rs",
                "wit_bindgen::generate!({ world: \"w\", path: WIT_DIR });",
            )],
        );
        let mut cg = ClassGraph::default();
        scan_rs(&repo, "c/src/lib.rs", "c", &mut cg);
        assert_eq!(cg.unresolved.len(), 1, "{:?}", cg.unresolved);
    }

    #[test]
    fn a_directory_claim_covers_its_bare_path() {
        let claims = vec!["external/testsuite/".to_string()];
        assert!(claimed("external/testsuite", &claims));
        assert!(claimed("external/testsuite/a.wast", &claims));
        assert!(!claimed("external/testsuite2", &claims));
    }

    #[test]
    fn a_manifest_dir_format_literal_is_a_runtime_read() {
        let repo = tree(
            "fmtlit",
            &[
                (
                    "crates/c/tests/t.rs",
                    "let p = format!(\"{}/../../test-data/x.aadl\", env!(\"CARGO_MANIFEST_DIR\"));",
                ),
                ("test-data/x.aadl", "x"),
            ],
        );
        let mut cg = ClassGraph::default();
        scan_rs(&repo, "crates/c/tests/t.rs", "crates/c", &mut cg);
        assert_eq!(
            cg.claims.get("test-data/x.aadl"),
            Some(&"literal"),
            "{:?}",
            cg.claims
        );
    }

    #[test]
    fn only_the_exact_out_dir_form_is_exempt() {
        assert!(is_out_dir_include(r#"concat!(env!("OUT_DIR"), "/gen.rs")"#));
        assert!(!is_out_dir_include(
            r#"concat!(env!("OUT_DIR"), "/../../../src/x.rs")"#
        ));
        assert!(!is_out_dir_include(
            r#"concat!(env!("OUT_DIR"), env!("X"))"#
        ));
        assert!(!is_out_dir_include(
            r#"concat!(env!("SRC"), "/x.rs", env!("OUT_DIR"))"#
        ));
    }

    #[test]
    fn an_out_dir_include_reaching_back_is_unresolved() {
        let repo = tree(
            "outdir",
            &[(
                "c/src/lib.rs",
                "include!(concat!(env!(\"OUT_DIR\"), \"/../../../../src/x.rs\"));\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));",
            )],
        );
        let mut cg = ClassGraph::default();
        scan_rs(&repo, "c/src/lib.rs", "c", &mut cg);
        assert_eq!(cg.unresolved.len(), 1, "{:?}", cg.unresolved);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_a_claim_claims_its_target() {
        let repo = tree(
            "symlink",
            &[("c/src/lib.rs", "mod x;"), ("other/x.rs", "pub fn x() {}")],
        );
        std::os::unix::fs::symlink("../../other/x.rs", repo.root.join("c/src/x.rs")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", repo.root.join("c/src/out.rs")).unwrap();
        let mut cg = ClassGraph::default();
        cg.add("c/".into(), "cargo");
        follow_symlinks(&repo, &mut cg);
        assert_eq!(
            cg.claims.get("other/x.rs"),
            Some(&"symlink"),
            "{:?}",
            cg.claims
        );
        assert_eq!(cg.unresolved.len(), 1, "{:?}", cg.unresolved);
    }

    #[test]
    fn top_level_path_skips_nested_braces() {
        let v = top_level_value(
            r#" world: "w", with: { "a:b/c": generate, path: "x" }, path: "../../wit/hal" "#,
            "path",
        );
        assert_eq!(v.map(str::trim), Some(r#""../../wit/hal""#));
    }
}
