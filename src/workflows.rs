//! `sieve check-workflows`: assert every required status check will actually
//! be produced on every pull request, and can be skipped only through the
//! classifier. Exit 0 pass, 1 a gate failed, 2 usage, 3 refused.
//!
//! A required context that never reports leaves a PR BLOCKED on a check that
//! will never arrive; a required job skipped by anything but the classifier
//! reports "skipped", which branch protection counts as passing.
//!
//!  (a) trigger state: the workflow hosting each required context triggers on
//!      `pull_request` with no `paths`/`paths-ignore` KEY (an empty one parses
//!      as null: ambiguous, and that is the point), no branch restriction that
//!      excludes main, no `types:` dropping a default activity, and also on
//!      `merge_group` (the queue waits on its own run).
//!  (b) a required job is skipped only by the classifier: its `if:` is absent
//!      or exactly
//!        !cancelled() && (needs.<C>.result != 'success' || needs.<C>.outputs.<o> != 'false')
//!      with <C> the job that runs `sieve classify ... --format github-output`,
//!      wired exactly, and <o> one of the outputs the HAND-MAINTAINED
//!      `[workflows.job_outputs]` map of sieve.toml gives that job; plus the
//!      hardening listed at `job_skip_state`.
//!  (c) the classifier fails OPEN: sieve's own classification, in process, on
//!      push, merge_group, no base, an undiffable base and an empty diff
//!      selects every class (the check-graph `fail-open` control).
//!
//! What this does NOT prove: that `[workflows.job_outputs]` maps each job to
//! the output that really covers its inputs, or that the classes' input sets
//! are complete. That map is HAND-MAINTAINED; the false-skip controls test
//! its content. Nor does it read the real branch protection (needs admin
//! rights): that direction is reported as UNGUARDED.

use crate::config::Config;
use regex::Regex;
use rivet_yaml::Value as Yaml;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_WORKFLOWS: &str = ".github/workflows";
const DEFAULT_REQUIRED: &str = ".github/required-contexts.txt";
/// A pull_request event fires on these activity types unless `types:` overrides it.
const DEFAULT_PR_TYPES: &[&str] = &["opened", "synchronize", "reopened"];
/// Read by the classifier; a workflow or job that sets one overrides the event.
const CLASSIFIER_ENV_KEYS: &[&str] = &[
    "EVENT_NAME",
    "BASE_SHA",
    "GATE_PROBE_FILES",
    "GITHUB_EVENT_NAME",
];
const CLASSIFIER_ENV: &[(&str, &str)] = &[
    ("EVENT_NAME", "${{ github.event_name }}"),
    ("BASE_SHA", "${{ github.event.pull_request.base.sha }}"),
];

// ---------------------------------------------------------------- YAML access

fn key_is(k: &Yaml, name: &str) -> bool {
    matches!(k, Yaml::String(s) if s == name)
}

fn get<'a>(y: &'a Yaml, name: &str) -> Option<&'a Yaml> {
    match y {
        Yaml::Mapping(h) => h.iter().find(|(k, _)| key_is(k, name)).map(|(_, v)| v),
        _ => None,
    }
}

fn has(y: &Yaml, name: &str) -> bool {
    get(y, name).is_some()
}

/// `x.get(k)` where a null counts as absent (Python's `is not None`).
fn get_nn<'a>(y: &'a Yaml, name: &str) -> Option<&'a Yaml> {
    get(y, name).filter(|v| !matches!(v, Yaml::Null))
}

fn entries(y: Option<&Yaml>) -> Vec<(String, &Yaml)> {
    match y {
        Some(Yaml::Mapping(h)) => h.iter().map(|(k, v)| (py_str(k), v)).collect(),
        _ => vec![],
    }
}

fn list(y: Option<&Yaml>) -> Vec<&Yaml> {
    match y {
        Some(Yaml::Sequence(a)) => a.iter().collect(),
        _ => vec![],
    }
}

/// Python `str()` of a scalar, close enough for messages and regex tests.
fn py_str(y: &Yaml) -> String {
    match y {
        Yaml::String(s) => s.clone(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Bool(true) => "True".into(),
        Yaml::Bool(false) => "False".into(),
        Yaml::Null => "None".into(),
        other => format!("{other:?}"),
    }
}

/// Every string anywhere under `y`, newline-joined (Python `json.dumps` search).
fn flatten(y: &Yaml, out: &mut String) {
    match y {
        Yaml::Sequence(a) => a.iter().for_each(|v| flatten(v, out)),
        Yaml::Mapping(h) => h.iter().for_each(|(k, v)| {
            flatten(k, out);
            flatten(v, out);
        }),
        other => {
            out.push_str(&py_str(other));
            out.push('\n');
        }
    }
}

fn truthy(y: &Yaml) -> bool {
    match y {
        Yaml::Null | Yaml::Bool(false) => false,
        Yaml::Number(n) => n.as_f64() != Some(0.0),
        Yaml::String(s) => !s.is_empty(),
        Yaml::Sequence(a) => !a.is_empty(),
        Yaml::Mapping(h) => !h.is_empty(),
        _ => true,
    }
}

/// The string members of a YAML scalar-or-list (a scalar is a one-element list).
fn members(y: &Yaml) -> Vec<String> {
    match y {
        Yaml::Sequence(a) => a.iter().map(py_str).collect(),
        other => vec![py_str(other)],
    }
}

fn show(y: &Yaml) -> String {
    members(y).join(", ")
}

// ------------------------------------------------------------- (a) triggers

type Problem = (&'static str, String);

/// Why (or whether) this workflow reports on every PR against main and in the
/// merge queue. `None` is fine.
fn trigger_state(wf: &Yaml) -> Option<Problem> {
    // YAML 1.1 parsers read the key `on` as boolean true; 1.2 ones as "on".
    let on = match wf {
        Yaml::Mapping(h) => h
            .iter()
            .find(|(k, _)| matches!(k, Yaml::Bool(true)))
            .map(|(_, v)| v),
        _ => None,
    }
    .or_else(|| get(wf, "on"));
    let no_mg = || {
        Some((
            "no-merge-group",
            "no merge_group trigger -- never reports in the merge queue".to_string(),
        ))
    };
    match on {
        Some(Yaml::String(s)) => {
            if s == "pull_request" {
                no_mg()
            } else {
                Some(("no-pr", format!("only `on: {s}`")))
            }
        }
        Some(Yaml::Sequence(a)) => {
            let names: Vec<String> = a.iter().map(py_str).collect();
            if !names.iter().any(|n| n == "pull_request") {
                Some(("no-pr", "no pull_request trigger".into()))
            } else if !names.iter().any(|n| n == "merge_group") {
                no_mg()
            } else {
                None
            }
        }
        Some(on @ Yaml::Mapping(_)) => {
            // sieve#25: a `paths:` on any trigger (push included) leaves the
            // full runs on main incomplete; the pull_request case is detailed below.
            if let Yaml::Mapping(h) = on {
                for (ev, spec) in h {
                    let ev = py_str(ev);
                    if ev == "pull_request" {
                        continue;
                    }
                    for k in ["paths", "paths-ignore"] {
                        if get(spec, k).is_some() {
                            return Some(("paths-filter", format!("`{ev}: {k}:` present")));
                        }
                    }
                }
            }
            let Some(pr) = get(on, "pull_request") else {
                return Some(("no-pr", "no pull_request trigger".into()));
            };
            // A bare `pull_request:` fires on everything; it still needs the queue trigger.
            if !matches!(pr, Yaml::Null) {
                if !matches!(pr, Yaml::Mapping(_)) {
                    return Some(("no-pr", format!("unparseable pull_request: {pr:?}")));
                }
                // Presence of the KEY is what matters, not its value (gale#340).
                for k in ["paths", "paths-ignore"] {
                    if let Some(v) = get(pr, k) {
                        let mut detail = format!("`{k}:` present");
                        match v {
                            Yaml::Null => detail.push_str(
                                " with no entries (parses as null -- ambiguous, and that is the point)",
                            ),
                            Yaml::Sequence(a) => detail.push_str(&format!(" ({} pattern(s))", a.len())),
                            _ => {}
                        }
                        return Some(("paths-filter", detail));
                    }
                }
                if let Some(b) = get_nn(pr, "branches") {
                    if !members(b).iter().any(|m| m == "main") {
                        return Some((
                            "branch-mismatch",
                            format!("branches=[{}] excludes main", show(b)),
                        ));
                    }
                }
                if let Some(b) = get(pr, "branches-ignore").filter(|b| truthy(b)) {
                    return Some(("branch-mismatch", format!("branches-ignore=[{}]", show(b))));
                }
                if let Some(t) = get_nn(pr, "types") {
                    let have = members(t);
                    let missing: Vec<&str> = DEFAULT_PR_TYPES
                        .iter()
                        .copied()
                        .filter(|d| !have.iter().any(|h| h == d))
                        .collect();
                    if !missing.is_empty() {
                        return Some((
                            "types-restricted",
                            format!("types=[{}] drops {missing:?}", show(t)),
                        ));
                    }
                }
            }
            // The queue runs required checks on `merge_group`, whose only filter
            // is `types:` and whose only activity type is checks_requested.
            let Some(mg) = get(on, "merge_group") else {
                return no_mg();
            };
            if let Some(t) = get_nn(mg, "types") {
                if !members(t).iter().any(|m| m == "checks_requested") {
                    return Some((
                        "no-merge-group",
                        format!("merge_group types=[{}] drops checks_requested", show(t)),
                    ));
                }
            }
            None
        }
        _ => Some(("no-pr", "unparseable `on:`".into())),
    }
}

// ---------------------------------------------------------------- the index

struct Wf {
    file: String,
    doc: Yaml,
    state: Option<Problem>,
}

struct Hit {
    wf: usize,
    jid: String,
}

fn job_name(job: &Yaml, jid: &str) -> String {
    match get(job, "name") {
        Some(n) if truthy(n) => py_str(n),
        _ => jid.to_string(),
    }
}

/// sieve#30: the check names a job produces. A `name:` with
/// `${{ matrix.<key> }}` expands once per matrix row, rows built as GitHub
/// does: the cartesian product of the list-valued keys, each `include` entry
/// added to every row whose original values it does not overwrite (or, if
/// none, as a row of its own), rows matching an `exclude` entry dropped. A
/// key a row lacks renders empty. A name with any other expression, or a
/// matrix computed from an expression, is returned as written (and stays
/// unmapped, which fails closed).
fn matrix_names(job: &Yaml, jid: &str) -> Vec<String> {
    let name = job_name(job, jid);
    let key_re = Regex::new(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}").unwrap();
    if !key_re.is_match(&name) {
        return vec![name];
    }
    if key_re.replace_all(&name, "").contains("${{") {
        return vec![name];
    }
    let Some(Yaml::Mapping(m)) = get(job, "strategy").and_then(|s| get(s, "matrix")) else {
        return vec![name];
    };
    // the cartesian product of the list-valued keys
    let mut rows: Vec<BTreeMap<String, String>> = vec![BTreeMap::new()];
    let mut originals: BTreeSet<String> = BTreeSet::new();
    for (k, v) in m {
        let k = py_str(k);
        if k == "include" || k == "exclude" {
            continue;
        }
        let Yaml::Sequence(vals) = v else {
            return vec![name]; // computed from an expression
        };
        originals.insert(k.clone());
        let mut next = Vec::new();
        for r in &rows {
            for x in vals {
                let mut r = r.clone();
                r.insert(k.clone(), py_str(x));
                next.push(r);
            }
        }
        rows = next;
    }
    let entry = |e: &Yaml| -> BTreeMap<String, String> {
        entries(Some(e))
            .into_iter()
            .map(|(k, v)| (k, py_str(v)))
            .collect()
    };
    if let Some(Yaml::Sequence(ex)) = get(&Yaml::Mapping(m.clone()), "exclude") {
        for e in ex.iter().map(entry) {
            rows.retain(|r| !e.iter().all(|(k, v)| r.get(k) == Some(v)));
        }
    }
    let includes: Vec<BTreeMap<String, String>> = match get(&Yaml::Mapping(m.clone()), "include") {
        Some(Yaml::Sequence(inc)) => inc.iter().map(entry).collect(),
        _ => vec![],
    };
    if originals.is_empty() {
        // only `include:`: each entry is a row
        rows = includes;
    } else {
        let base = rows.len();
        for e in includes {
            let mut added = false;
            for r in rows.iter_mut().take(base) {
                let overwrites = e
                    .iter()
                    .any(|(k, v)| originals.contains(k) && r.get(k).is_some_and(|x| x != v));
                if !overwrites {
                    r.extend(e.clone());
                    added = true;
                }
            }
            if !added {
                rows.push(e);
            }
        }
    }
    if rows.is_empty() {
        return vec![name];
    }
    rows.iter()
        .map(|r| {
            key_re
                .replace_all(&name, |c: &regex::Captures| {
                    r.get(&c[1]).cloned().unwrap_or_default()
                })
                .into_owned()
        })
        .collect()
}

/// An if: that keeps a job (or step) running: exactly always() or !cancelled(),
/// optionally inside one `${{ }}` wrapper.
fn unconditional_if(cond: &Yaml) -> bool {
    let raw = py_str(cond);
    let mut c = raw.trim();
    if let Some(inner) = c.strip_prefix("${{").and_then(|x| x.strip_suffix("}}")) {
        c = inner.trim();
    }
    c == "always()" || c == "!cancelled()"
}

/// Why a never-skip job could be skipped or emptied, if it could (REQ-028).
fn never_skip_problem(
    state: &Option<Problem>,
    jobs: Option<&Yaml>,
    jid: &str,
    job: &Yaml,
) -> Option<(String, String)> {
    if let Some((code, detail)) = state {
        return Some((format!("never-skip-trigger-{code}"), detail.clone()));
    }
    if get(job, "uses").is_some() {
        return Some((
            "never-skip-reusable".into(),
            "a guard job calls a reusable workflow; its callee is not checked".into(),
        ));
    }
    if let Some(c) = get(job, "if") {
        if !unconditional_if(c) {
            return Some((
                "never-skip-if".into(),
                format!(
                    "a guard job must not be skippable; its if: is `{}`",
                    py_str(c)
                ),
            ));
        }
    }
    if get(job, "continue-on-error").is_some_and(truthy) {
        return Some((
            "never-skip-continue-on-error".into(),
            "a guard job must not swallow its own failure".into(),
        ));
    }
    if let Some(st) = get(job, "strategy") {
        let bad = |why: &str| Some(("never-skip-matrix".to_string(), why.to_string()));
        if !matches!(st, Yaml::Mapping(_)) {
            return bad("strategy is not a literal mapping");
        }
        if let Some(m) = get(st, "matrix") {
            if !matches!(m, Yaml::Mapping(_)) {
                return bad("the matrix is an expression, not a literal");
            }
            let mut combos = false;
            for (k, v) in entries(Some(m)) {
                match k.as_str() {
                    // an exclude can remove every combination: fail closed
                    "exclude" => return bad("a guard job's matrix must not use exclude"),
                    "include" => match v {
                        Yaml::Sequence(a) => combos |= !a.is_empty(),
                        _ => return bad("matrix include is not a literal list"),
                    },
                    _ => match v {
                        Yaml::Sequence(a) if !a.is_empty() => combos = true,
                        _ => {
                            return bad(&format!(
                                "matrix axis `{k}` is not a non-empty literal list"
                            ))
                        }
                    },
                }
            }
            if !combos {
                return bad("the matrix has no combination");
            }
        }
    }
    for st in list(get(job, "steps")) {
        if let Some(c) = get(st, "if") {
            if !unconditional_if(c) {
                return Some((
                    "never-skip-step-if".into(),
                    format!("a step of a guard job has if: `{}`", py_str(c)),
                ));
            }
        }
    }
    // every job it needs, transitively, must exist and be unconditional
    let mut seen = BTreeSet::new();
    let mut todo: Vec<String> = get(job, "needs").map(members).unwrap_or_default();
    // A failed need skips a job whose if: is the implicit success(), so a
    // guard job with needs must itself be always() or !cancelled().
    if !todo.is_empty() && !get(job, "if").is_some_and(unconditional_if) {
        return Some((
            "never-skip-needs".into(),
            "a guard job with needs: must have if: always() or !cancelled(); otherwise a failed need skips it".into(),
        ));
    }
    while let Some(n) = todo.pop() {
        if n == jid || !seen.insert(n.clone()) {
            continue;
        }
        let Some(nj) = jobs.and_then(|j| get(j, &n)) else {
            return Some((
                "never-skip-needs".into(),
                format!("needs `{n}`, which no job in this workflow defines"),
            ));
        };
        if get(nj, "if").is_some_and(|c| !unconditional_if(c)) {
            return Some(("never-skip-needs".into(), format!("needs `{n}` (directly or through another need), which has its own if: and can be skipped; the skip would propagate")));
        }
        todo.extend(get(nj, "needs").map(members).unwrap_or_default());
    }
    None
}

/// A cargo invocation in a gated job that names a package or a manifest.
struct CargoBuild {
    file: String,
    jid: String,
    job: String,
    classes: Vec<String>,
    /// `-p X` / `--package X`: the package name; otherwise the manifest path
    package: Option<String>,
    manifest: Option<String>,
    /// repository-relative working directory of the step ("" = root)
    wd: String,
}

fn cargo_builds(wfs: &[Wf], cfg: &Config) -> Vec<CargoBuild> {
    let cmd = Regex::new(
        r"\bcargo\s+(?:\+\S+\s+)?(?:build|run|test|check|clippy|nextest|llvm-cov|kani|miri|bench|doc)\b[^\n;&|]*",
    )
    .unwrap();
    let pkg = Regex::new(r#"(?:^|\s)(?:-p|--package)(?:\s+|=)['"]?([^\s'"]+)"#).unwrap();
    let man = Regex::new(r#"(?:^|\s)--manifest-path(?:\s+|=)['"]?([^\s'"]+)"#).unwrap();
    let mut out = vec![];
    for wf in wfs {
        for (jid, job) in entries(get(&wf.doc, "jobs")) {
            let name = job_name(job, &jid);
            let Some(classes) = cfg.workflows.job_outputs.get(&name) else {
                continue; // ungated: it always runs
            };
            let job_wd = get(job, "defaults")
                .and_then(|d| get(d, "run"))
                .and_then(|r| get(r, "working-directory"))
                .map(py_str);
            for st in list(get(job, "steps")) {
                let run = run_of(st);
                let wd = get(st, "working-directory")
                    .map(py_str)
                    .or_else(|| job_wd.clone())
                    .unwrap_or_default();
                for m in cmd.find_iter(&run) {
                    let mk = |package: Option<String>, manifest: Option<String>| CargoBuild {
                        file: wf.file.clone(),
                        jid: jid.clone(),
                        job: name.clone(),
                        classes: classes.clone(),
                        package,
                        manifest,
                        wd: wd.clone(),
                    };
                    for c in pkg.captures_iter(m.as_str()) {
                        out.push(mk(Some(c[1].to_string()), None));
                    }
                    for c in man.captures_iter(m.as_str()) {
                        out.push(mk(None, Some(c[1].to_string())));
                    }
                }
            }
        }
    }
    out
}

/// The claims sieve's graph gives a class whose only Cargo root is `manifest`:
/// what `cargo build|test|run -p` of that package reads.
fn built_closure(
    repo: &crate::repo::Repo,
    cfg: &Config,
    manifest: &str,
) -> Result<Vec<String>, String> {
    let mut one = cfg.clone();
    one.classes = vec![crate::config::Class {
        name: "__built__".into(),
        cargo: vec![manifest.to_string()],
        ..Default::default()
    }];
    let g = crate::graph::derive(repo, &one).map_err(|e| e.message)?;
    let cg = g.get("__built__").ok_or("no closure derived")?;
    if let Some(u) = cg.unresolved.first() {
        return Err(format!("unresolved input {u}"));
    }
    Ok(cg.claims.keys().cloned().collect())
}

fn cross_class_check(r: &mut Report, builds: &[CargoBuild], root: &Path, cfg: &Config) {
    let repo = crate::repo::Repo {
        root: root.to_path_buf(),
        git: root.join(".git").exists(),
    };
    let tracked = match repo.tracked() {
        Ok(t) => t,
        Err(e) => return r.refuse("d", format!("cross-class check cannot list files: {e}")),
    };
    // package name -> manifest, from the repository's own Cargo.toml files
    let mut packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in tracked
        .iter()
        .filter(|f| crate::repo::basename(f) == "Cargo.toml")
    {
        let Ok(text) = repo.read(f) else { continue };
        let Ok(v) = text.parse::<toml::Value>() else {
            continue;
        };
        if let Some(n) = v
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            packages.entry(n.to_string()).or_default().push(f.clone());
        }
    }
    let graph = match crate::graph::derive(&repo, cfg) {
        Ok(g) => g,
        Err(e) => {
            return r.refuse(
                "d",
                format!("cross-class check cannot derive the graph: {}", e.message),
            )
        }
    };
    for b in builds {
        r.n += 1;
        let ctx = format!("cross-class {}", b.job);
        let what = b.package.as_ref().map_or_else(
            || format!("--manifest-path {}", b.manifest.as_deref().unwrap_or("")),
            |p| format!("-p {p}"),
        );
        if what.contains("${{") || b.wd.contains("${{") {
            r.fail("d", &ctx, &b.file, &b.jid, "cross-class-not-literal",
                format!("`{what}` (working directory `{}`) is an expression; the package it builds cannot be checked", b.wd));
            continue;
        }
        let manifests: Vec<String> = match (&b.package, &b.manifest) {
            (Some(p), _) => {
                // `-p name@1.2.3` / `-p name:1.2.3` name a version of the same package
                let name = p.split(['@', ':']).next().unwrap_or(p);
                match packages.get(name) {
                    Some(m) => m.clone(),
                    None => {
                        r.fail(
                            "d",
                            &ctx,
                            &b.file,
                            &b.jid,
                            "cross-class-unresolved",
                            format!("`{what}`: no tracked Cargo.toml names package `{name}`"),
                        );
                        continue;
                    }
                }
            }
            (None, Some(m)) => {
                let base = b.wd.trim_start_matches("./").trim_end_matches('/');
                match crate::repo::norm(base, m) {
                    Some(p) => vec![p],
                    None => {
                        r.fail(
                            "d",
                            &ctx,
                            &b.file,
                            &b.jid,
                            "cross-class-unresolved",
                            format!("`{what}` leaves the repository"),
                        );
                        continue;
                    }
                }
            }
            (None, None) => continue,
        };
        // What the command builds is the package's full Cargo closure as sieve
        // derives it for a root (all targets, dev-dependencies, path deps,
        // literal reads), not just its manifest. Every claim of that closure
        // must be covered by one of the job's classes. A package name shared
        // by several manifests needs each closure covered.
        let covered = |p: &str| {
            b.classes
                .iter()
                .any(|c| graph.get(c).is_some_and(|g| g.claims_path(p)))
        };
        let mut unclaimed: Vec<String> = vec![];
        let mut failed_closure = None;
        for m in &manifests {
            match built_closure(&repo, cfg, m) {
                Ok(claims) => unclaimed.extend(claims.into_iter().filter(|c| !covered(c))),
                Err(e) => failed_closure = Some(format!("{m}: {e}")),
            }
        }
        if let Some(e) = failed_closure {
            r.fail(
                "d",
                &ctx,
                &b.file,
                &b.jid,
                "cross-class-unresolved",
                format!("`{what}`: the closure it builds cannot be derived ({e})"),
            );
        } else if unclaimed.is_empty() {
            r.text.push(format!(
                "    claims         {} builds {what}  [{}]",
                b.job, b.file
            ));
        } else {
            unclaimed.sort();
            unclaimed.dedup();
            let shown: Vec<&String> = unclaimed.iter().take(5).collect();
            r.fail("d", &ctx, &b.file, &b.jid, "cross-class-build",
                format!("runs `{what}` but its classes {:?} do not claim {} input(s) it builds, e.g. {:?}; a change there would skip this job", b.classes, unclaimed.len(), shown));
        }
    }
}

// --------------------------------------------------- (b) skip only via sieve

fn needs_result() -> Regex {
    Regex::new(r"^needs\.([A-Za-z0-9_-]+)\.result\s*!=\s*'success'$").unwrap()
}
fn atom() -> Regex {
    Regex::new(r"^needs\.([A-Za-z0-9_-]+)\.outputs\.([A-Za-z0-9_-]+)\s*!=\s*'false'$").unwrap()
}
fn mentions_classifier(run: &str) -> bool {
    Regex::new(r"\bsieve\s+classify\b").unwrap().is_match(run)
}

fn run_of(st: &Yaml) -> String {
    get_nn(st, "run").map(py_str).unwrap_or_default()
}

/// {job id -> (classifier step, output names wired to that step)} for every
/// job with a step that mentions the classifier (exactness is checked apart).
fn classifier_jobs(wf: &Yaml) -> BTreeMap<String, (&Yaml, BTreeSet<String>)> {
    let mut out = BTreeMap::new();
    for (jid, job) in entries(get(wf, "jobs")) {
        for st in list(get(job, "steps")) {
            if !mentions_classifier(&run_of(st)) {
                continue;
            }
            let sid = get_nn(st, "id").map(py_str);
            let mut outs = BTreeSet::new();
            for (k, v) in entries(get(job, "outputs")) {
                if let Some(sid) = &sid {
                    let re = Regex::new(&format!(
                        r"^\$\{{\{{\s*steps\.{}\.outputs\.{}\s*\}}\}}$",
                        regex::escape(sid),
                        regex::escape(&k)
                    ))
                    .unwrap();
                    if re.is_match(py_str(v).trim()) {
                        outs.insert(k);
                    }
                }
            }
            out.insert(jid.clone(), (st, outs));
        }
    }
    out
}

/// Whether `run:` is exactly `sieve classify --config <this config> --base
/// "$BASE_SHA" --event "$EVENT_NAME" --format github-output`, optionally with
/// `--record <path>` (sieve#25, see [record_path_ok]); flags in any order, the
/// program optionally given as a path ending in `sieve`.
fn run_problem(run: &str, cfg_path: &str) -> Option<String> {
    let want = format!(
        "sieve classify --config {cfg_path} --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output"
    );
    let bad = || {
        Some(format!(
            "`run:` is {:?}, not exactly {want:?} (optionally with --record $RUNNER_TEMP/<file>)",
            run.trim()
        ))
    };
    let run = run.trim();
    // Chaining, redirection, substitution, continuation and single quotes
    // cannot survive the token match below: each glues onto a token or adds an
    // unknown one, and only the exact flag values are accepted.
    if run.is_empty() {
        return bad();
    }
    let mut toks: Vec<String> = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut in_tok = false;
    for ch in run.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                in_tok = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_tok {
                    toks.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            c => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if quoted {
        return bad();
    }
    if in_tok {
        toks.push(cur);
    }
    let prog_ok = toks.first().is_some_and(|p| {
        p.rsplit('/').next() == Some("sieve")
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    });
    if !prog_ok || toks.get(1).map(String::as_str) != Some("classify") {
        return bad();
    }
    let norm = |p: &str| p.strip_prefix("./").unwrap_or(p).to_string();
    let (mut config, mut base, mut event, mut format, mut record) = (None, None, None, None, None);
    let mut i = 2;
    while i < toks.len() {
        let (flag, val) = (toks[i].as_str(), toks.get(i + 1));
        let slot = match flag {
            "--config" => &mut config,
            "--base" => &mut base,
            "--event" => &mut event,
            "--format" => &mut format,
            "--record" => &mut record,
            _ => return bad(),
        };
        let Some(v) = val else { return bad() };
        if slot.replace(v.clone()).is_some() {
            return bad();
        }
        i += 2;
    }
    let base_ok = matches!(base.as_deref(), Some("$BASE_SHA" | "${BASE_SHA}"));
    let event_ok = matches!(event.as_deref(), Some("$EVENT_NAME" | "${EVENT_NAME}"));
    let config_ok = config.as_deref().map(norm).as_deref() == Some(cfg_path);
    if base_ok
        && event_ok
        && config_ok
        && format.as_deref() == Some("github-output")
        && record.as_deref().map_or(true, record_path_ok)
    {
        None
    } else {
        bad()
    }
}

/// A `--record` value the gate accepts (sieve#25): `$RUNNER_TEMP/<name>` or a
/// relative path, made of `[A-Za-z0-9_.-/]` with no `..` segment. No other
/// variable, so the verdict cannot be pointed at $GITHUB_OUTPUT or $GITHUB_ENV.
fn record_path_ok(v: &str) -> bool {
    let rest = v
        .strip_prefix("$RUNNER_TEMP/")
        .or_else(|| v.strip_prefix("${RUNNER_TEMP}/"))
        .unwrap_or(v);
    !rest.is_empty()
        && !rest.starts_with('/')
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-/".contains(c))
        && rest.split('/').all(|s| !s.is_empty() && s != "..")
}

/// [problems] with how classifier job `cjid` runs the classifier.
fn classifier_wiring(wf: &Yaml, cjid: &str, cfg_path: &str) -> Vec<String> {
    let jobs = entries(get(wf, "jobs"));
    let job = jobs
        .iter()
        .find(|(j, _)| j == cjid)
        .map(|(_, y)| *y)
        .unwrap_or(&Yaml::Null);
    let steps: Vec<&Yaml> = list(get(job, "steps"))
        .into_iter()
        .filter(|s| mentions_classifier(&run_of(s)))
        .collect();
    if steps.len() != 1 {
        return vec![format!(
            "{cjid}: {} steps mention `sieve classify`; exactly one must run it",
            steps.len()
        )];
    }
    let st = steps[0];
    let mut probs = vec![];
    if let Some(p) = run_problem(&run_of(st), cfg_path) {
        probs.push(format!("{cjid}: classifier step {p}"));
    }
    let env: BTreeMap<String, String> = entries(get(st, "env"))
        .into_iter()
        .map(|(k, v)| (k, py_str(v).trim().to_string()))
        .collect();
    let want: BTreeMap<String, String> = CLASSIFIER_ENV
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if env != want {
        probs.push(format!(
            "{cjid}: classifier step `env:` is {env:?}, not exactly {want:?}"
        ));
    }
    for k in ["if", "continue-on-error"] {
        if has(st, k) {
            probs.push(format!("{cjid}: classifier step has `{k}:`"));
        }
        if has(job, k) {
            probs.push(format!("{cjid}: classifier job has `{k}:`"));
        }
    }
    for (where_, e) in [("job", get(job, "env")), ("workflow", get(wf, "env"))] {
        let keys: BTreeSet<String> = entries(e).into_iter().map(|(k, _)| k).collect();
        let hit: Vec<&&str> = CLASSIFIER_ENV_KEYS
            .iter()
            .filter(|k| keys.contains(**k))
            .collect();
        if !hit.is_empty() {
            probs.push(format!(
                "{cjid}: {where_}-level env sets {hit:?}, which the classifier reads"
            ));
        }
    }
    if get_nn(st, "id").is_none() {
        probs.push(format!(
            "{cjid}: classifier step has no id, so no output can be wired to it"
        ));
    }
    probs
}

fn split_top(expr: &str, op: &str) -> Vec<String> {
    let chars: Vec<char> = expr.chars().collect();
    let op: Vec<char> = op.chars().collect();
    let (mut parts, mut depth, mut cur, mut i) = (vec![], 0i32, String::new(), 0);
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && chars[i..].starts_with(&op) {
            parts.push(cur.trim().to_string());
            cur.clear();
            i += op.len();
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }
    parts.push(cur.trim().to_string());
    parts
}

/// Remove ONE pair of parentheses enclosing the whole expression, if any.
fn strip_parens(e: &str) -> (String, bool) {
    let e = e.trim();
    if !(e.starts_with('(') && e.ends_with(')')) {
        return (e.to_string(), false);
    }
    let mut depth = 0i32;
    let n = e.chars().count();
    for (i, ch) in e.chars().enumerate() {
        depth += (ch == '(') as i32;
        depth -= (ch == ')') as i32;
        if depth == 0 && i < n - 1 {
            return (e.to_string(), false);
        }
    }
    (e[1..e.len() - 1].trim().to_string(), true)
}

/// (classifier job, outputs) if `cond` is exactly the accepted form, else why not.
fn parse_gate_if(cond: &str) -> Result<(String, Vec<String>), String> {
    let mut e = cond.trim();
    if e.starts_with("${{") && e.ends_with("}}") && e.len() >= 5 {
        e = e[3..e.len() - 2].trim();
    }
    let conj = split_top(e, "&&");
    if conj.len() != 2 || conj[0] != "!cancelled()" {
        return Err(format!(
            "`if: {cond}` is not `!cancelled() && (needs.<C>.result != 'success' || \
needs.<C>.outputs.<o> != 'false')`"
        ));
    }
    let (inner, had) = strip_parens(&conj[1]);
    if !had {
        return Err(format!(
            "`if:` second term `{}` is not the parenthesised classifier disjunction",
            conj[1]
        ));
    }
    let terms: Vec<String> = split_top(&inner, "||")
        .iter()
        .map(|t| strip_parens(t).0)
        .collect();
    let (res_re, atom_re) = (needs_result(), atom());
    let res: Vec<_> = terms.iter().map(|t| res_re.captures(t)).collect();
    let atoms: Vec<_> = terms.iter().map(|t| atom_re.captures(t)).collect();
    if res.iter().filter(|m| m.is_some()).count() != 1 {
        return Err(
            "`if:` lacks exactly one `needs.<C>.result != 'success'`: a classifier that \
wrote false and then failed would skip the job"
                .into(),
        );
    }
    for ((t, r), a) in terms.iter().zip(&res).zip(&atoms) {
        if r.is_none() && a.is_none() {
            return Err(format!(
                "`if:` term `{t}` can skip it for a reason other than the classifier"
            ));
        }
    }
    let cs: BTreeSet<String> = res
        .iter()
        .chain(atoms.iter())
        .flatten()
        .map(|m| m[1].to_string())
        .collect();
    if cs.len() != 1 {
        return Err(format!(
            "`if:` mixes classifier jobs {:?}",
            cs.iter().collect::<Vec<_>>()
        ));
    }
    let outs: Vec<String> = atoms.iter().flatten().map(|a| a[2].to_string()).collect();
    if outs.is_empty() {
        return Err("`if:` reads no classifier output".into());
    }
    Ok((cs.into_iter().next().unwrap(), outs))
}

struct Skip {
    problem: Option<Problem>,
    /// (classifier job, output) pairs this job reads.
    used: Vec<(String, String)>,
}

/// Every way a required job can be skipped other than through the classifier:
///  - job `if:`: absent, or exactly the gated form, wired to the classifier
///    step and reading only the outputs `[workflows.job_outputs]` gives it;
///  - `needs:` only classifier jobs and other required jobs; a need that has
///    an `if:` of its own can be skipped, which skips this job through the
///    implicit success(), unless this job carries the classifier form;
///  - no step `if:` reading `needs.*` or `steps.*.outputs` (gale#319);
///  - no `continue-on-error` on the job; on a step only if a later step reads
///    its outcome/conclusion or it is on `[workflows.coe_exempt]`;
///  - a matrix this check can prove non-empty;
///  - the classifier step: exact `run:`/`env:`, no `if:`/`continue-on-error`
///    on step or job, no EVENT_NAME/BASE_SHA/GATE_PROBE_FILES in job- or
///    workflow-level env.
fn job_skip_state(cfg: &Config, wf: &Wf, jid: &str, required_jids: &BTreeSet<String>) -> Skip {
    let skippable = |code: &'static str, msg: String, used: Vec<(String, String)>| Skip {
        problem: Some((code, msg)),
        used,
    };
    let jobs = entries(get(&wf.doc, "jobs"));
    let job = jobs
        .iter()
        .find(|(j, _)| j == jid)
        .map(|(_, y)| *y)
        .unwrap_or(&Yaml::Null);
    let name = job_name(job, jid);
    let cls = classifier_jobs(&wf.doc);
    let needs: Vec<String> = match get(job, "needs") {
        Some(Yaml::String(s)) => vec![s.clone()],
        Some(Yaml::Sequence(a)) => a.iter().map(py_str).collect(),
        _ => vec![],
    };
    let cond = get_nn(job, "if");
    let mut used = vec![];

    if has(job, "continue-on-error") {
        return skippable(
            "continue-on-error-job",
            "`continue-on-error` on a required job: it reports success whatever fails".into(),
            used,
        );
    }
    let steps = list(get(job, "steps"));
    // Contexts are case-insensitive and also read by index (needs['x']).
    let step_if = Regex::new(
        r"(?i)\bneeds\s*[.\[]|\bsteps\s*(\.\s*[A-Za-z0-9_-]+|\[[^\]]*\])\s*\.\s*outputs\b|\btojson\s*\(\s*(needs|steps)\s*\)",
    )
    .unwrap();
    for (i, st) in steps.iter().enumerate() {
        let label = get_nn(st, "name")
            .map(py_str)
            .unwrap_or_else(|| i.to_string());
        let sif = get(st, "if").map(py_str).unwrap_or_default();
        if step_if.is_match(&sif) {
            return skippable(
                "step-if-output",
                format!(
                    "step {label} has `if: {sif}`: a step-level skip reports the job green having checked nothing"
                ),
                used,
            );
        }
        if has(st, "continue-on-error") {
            let observed = get_nn(st, "id").is_some_and(|sid| {
                let mut later = String::new();
                steps[i + 1..].iter().for_each(|s| flatten(s, &mut later));
                Regex::new(&format!(
                    r"steps\.{}\.(outcome|conclusion)",
                    regex::escape(&py_str(sid))
                ))
                .unwrap()
                .is_match(&later)
            });
            let exempt = cfg.workflows.coe_exempt.contains_key(&format!(
                "{}:{jid}:{}",
                wf.file,
                get_nn(st, "name").map(py_str).unwrap_or_default()
            ));
            if !observed && !exempt {
                return skippable(
                    "continue-on-error-step",
                    format!(
                        "step {label} has `continue-on-error` and no later step reads its outcome (nor is it on [workflows.coe_exempt])"
                    ),
                    used,
                );
            }
        }
    }

    // sieve#30: exactly always() or !cancelled() never skips the job.
    if let Some(cond) = cond.filter(|c| !unconditional_if(c)) {
        let Yaml::String(cond) = cond else {
            return skippable(
                "if-form",
                format!("`if: {}` is not the classifier form", py_str(cond)),
                used,
            );
        };
        let (c, outs) = match parse_gate_if(cond) {
            Ok(x) => x,
            Err(why) => return skippable("if-form", why, used),
        };
        let Some((_, wired)) = cls.get(&c) else {
            return skippable(
                "classifier-not-run",
                format!("`if:` reads needs.{c}, which does not run `sieve classify`"),
                used,
            );
        };
        if !needs.contains(&c) {
            return skippable(
                "classifier-not-needed",
                format!("`if:` reads needs.{c} but the job does not need it"),
                used,
            );
        }
        let probs = classifier_wiring(&wf.doc, &c, &cfg.path);
        if !probs.is_empty() {
            return skippable("classifier-wiring", probs.join("; "), used);
        }
        for o in &outs {
            if !wired.contains(o) {
                return skippable(
                    "output-unwired",
                    format!("`if:` reads output {o:?}, which job {c} does not wire to the classifier step"),
                    used,
                );
            }
        }
        // a matrix name (`Test (${{ matrix.label }})`) maps under its base name too
        let base = Regex::new(r"\s*\(.*\)\s*$")
            .unwrap()
            .replace(&name, "")
            .into_owned();
        let Some(allowed) = cfg
            .workflows
            .job_outputs
            .get(&name)
            .or_else(|| cfg.workflows.job_outputs.get(&base))
        else {
            return skippable(
                "job-not-mapped",
                "gated by the classifier but not on the hand-maintained [workflows.job_outputs] map"
                    .into(),
                used,
            );
        };
        let stray: BTreeSet<&String> = outs.iter().filter(|o| !allowed.contains(o)).collect();
        if !stray.is_empty() {
            return skippable(
                "output-not-allowed",
                format!("reads {stray:?}, another job's classifier output ([workflows.job_outputs] allows {allowed:?})"),
                used,
            );
        }
        used = outs.iter().map(|o| (c.clone(), o.clone())).collect();
    }

    for n in &needs {
        if cls.contains_key(n) {
            if cond.is_none() {
                return skippable(
                    "needs-classifier-without-if",
                    format!("needs classifier {n} without the classifier `if:`: a failed classifier skips it"),
                    used,
                );
            }
            continue;
        }
        if !required_jids.contains(n) {
            // An always()/!cancelled() job runs whatever its need did, so it is
            // not skipped; it must still check the need's result itself, or a
            // failed or skipped need lets it report success (relay's gates).
            if cond.is_some_and(unconditional_if) {
                let mut text = String::new();
                flatten(job, &mut text);
                let reads = Regex::new(&format!(
                    r"(?i)needs\s*(\.\s*{n}|\[\s*['\x22]{n}['\x22]\s*\])\s*\.\s*result|tojson\(\s*needs\s*\)",
                    n = regex::escape(n)
                ))
                .unwrap();
                if reads.is_match(&text) {
                    continue;
                }
                return skippable(
                    "needs-result-unread",
                    format!("needs {n}, which is not itself required, and no step reads needs.{n}.result: with `if:` always()/!cancelled() it runs when {n} fails or skips, and reports success unless it checks"),
                    used,
                );
            }
            return skippable(
                "needs-not-required",
                format!("needs {n}, which is not itself required: if {n} fails or skips, this job is skipped and reports success"),
                used,
            );
        }
        let need_job = jobs.iter().find(|(j, _)| j == n).map(|(_, y)| *y);
        if need_job.is_some_and(|nj| get_nn(nj, "if").is_some()) && cond.is_none() {
            return skippable(
                "needs-gated-job",
                format!("needs {n}, which has its own `if:`: when {n} is skipped this job is skipped too (implicit success()), unless it carries the classifier form"),
                used,
            );
        }
    }

    if let Some(m) = get(job, "strategy").and_then(|s| get_nn(s, "matrix")) {
        let bad = match m {
            Yaml::Mapping(h) => {
                get(m, "exclude").is_some_and(truthy)
                    || h.iter().any(|(k, v)| {
                        !key_is(k, "include") && !matches!(v, Yaml::Sequence(a) if !a.is_empty())
                    })
                    || (h.len() == 1 && get(m, "include").is_some_and(|i| !truthy(i)))
            }
            _ => true,
        };
        if bad {
            return skippable(
                "matrix-unprovable",
                "a matrix this check cannot prove non-empty (expression, `exclude:`, or empty axis)"
                    .into(),
                used,
            );
        }
    }
    Skip {
        problem: None,
        used,
    }
}

// ------------------------------------------------- REQ-030 leaf-workflow scope

/// Job keys that decide whether the job, or a job that needs it, runs.
const GATING_KEYS: &[&str] = &[
    "if",
    "needs",
    "continue-on-error",
    "strategy",
    "uses",
    "outputs",
];

fn needs_of(job: &Yaml) -> Vec<String> {
    match get(job, "needs") {
        None | Some(Yaml::Null) => vec![],
        Some(Yaml::Sequence(a)) => a.iter().map(py_str).collect(),
        Some(other) => vec![py_str(other)],
    }
}

fn on_of(wf: &Yaml) -> Option<&Yaml> {
    match wf {
        Yaml::Mapping(h) => h
            .iter()
            .find(|(k, _)| matches!(k, Yaml::Bool(true)))
            .map(|(_, v)| v),
        _ => None,
    }
    .or_else(|| get(wf, "on"))
}

/// A YAML merge key anywhere below `y`: `get` does not follow it, so a gating
/// key could hide behind one.
fn has_merge_key(y: &Yaml) -> bool {
    match y {
        Yaml::Mapping(h) => h.iter().any(|(k, v)| key_is(k, "<<") || has_merge_key(v)),
        Yaml::Sequence(a) => a.iter().any(has_merge_key),
        _ => false,
    }
}

/// The steps that carry an `if:` or `continue-on-error:`: the `if:` of each,
/// and the whole step where it has `continue-on-error:` (moving a command into
/// or out of such a step changes whether its failure fails the job).
fn step_gates(job: &Yaml) -> Vec<(Option<&Yaml>, Option<&Yaml>)> {
    list(get(job, "steps"))
        .into_iter()
        .filter(|s| has(s, "if") || has(s, "continue-on-error"))
        .map(|s| (get(s, "if"), has(s, "continue-on-error").then_some(s)))
        .collect()
}

/// A step `if:` that reads a context an edit (or another job) can change:
/// anything but `github`, `runner` and the status functions. Contexts are
/// case-insensitive and also read as `needs['x']` or `toJSON(needs)`.
// Every `uses:` value anywhere below `y` (as parsed YAML, so a quoted
// key or a folded scalar is seen).
fn collect_uses(y: &Yaml, out: &mut Vec<String>) {
    match y {
        Yaml::Mapping(h) => {
            for (k, v) in h {
                if key_is(k, "uses") {
                    out.push(py_str(v).trim().to_string());
                }
                collect_uses(v, out);
            }
        }
        Yaml::Sequence(a) => a.iter().for_each(|v| collect_uses(v, out)),
        _ => {}
    }
}

/// sieve#37: whether `y` reads the `needs` context. A context is read only
/// inside `${{ … }}`, or in an `if:` value (an expression without the
/// braces), so a shell comment saying "needs" does not count. An unclosed
/// `${{` counts as reading it (fails closed). Keys in `skip` are ignored at
/// the top level only (a job's own `needs:` list, and its `if:` where that
/// is checked elsewhere).
fn reads_needs(y: &Yaml, skip: &[&str]) -> bool {
    fn expr_reads(s: &str) -> bool {
        let word = Regex::new(r"(?i)\bneeds\b").unwrap();
        let mut rest = s;
        while let Some(i) = rest.find("${{") {
            let after = &rest[i + 3..];
            // The closing `}}` outside a '...' literal ('' escapes a quote),
            // as GitHub reads it; none means unclosed, which counts.
            let b = after.as_bytes();
            let (mut k, mut in_str, mut end) = (0, false, None);
            while k < b.len() {
                match (in_str, b[k]) {
                    (true, b'\'') if b.get(k + 1) == Some(&b'\'') => k += 1,
                    (_, b'\'') => in_str = !in_str,
                    (false, b'}') if b.get(k + 1) == Some(&b'}') => {
                        end = Some(k);
                        break;
                    }
                    _ => {}
                }
                k += 1;
            }
            let Some(j) = end else {
                return true;
            };
            if word.is_match(&after[..j]) {
                return true;
            }
            rest = &after[j + 2..];
        }
        false
    }
    fn walk(y: &Yaml, skip: &[&str]) -> bool {
        match y {
            Yaml::Mapping(h) => h.iter().any(|(k, v)| {
                if skip.iter().any(|s| key_is(k, s)) {
                    return false;
                }
                if key_is(k, "if") {
                    return Regex::new(r"(?i)\bneeds\b").unwrap().is_match(&py_str(v));
                }
                walk(v, &[])
            }),
            Yaml::Sequence(a) => a.iter().any(|v| walk(v, &[])),
            other => expr_reads(&py_str(other)),
        }
    }
    walk(y, skip)
}

/// `get` with the key matched without case (action inputs are case-insensitive).
fn get_ci<'a>(y: &'a Yaml, name: &str) -> Option<&'a Yaml> {
    match y {
        Yaml::Mapping(h) => h
            .iter()
            .find(|(k, _)| py_str(k).eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, Yaml::Null)),
        _ => None,
    }
}

/// sieve#36: each step's local `uses:` values, rewritten through the job's
/// earlier same-repository `actions/checkout` steps: after a checkout with
/// `with: path: gale`, `./gale/.github/actions/x` is the repository's own
/// `./.github/actions/x`. Fails closed (Err) on a local `uses:` under a path
/// checked out from another repository, after a checkout whose path is an
/// expression, or when another repository was checked out at the root. The
/// job-level `uses:` (a reusable workflow) is returned as is.
fn checkout_seeds(job: &Yaml, wf_default_wd: &str) -> Result<Vec<Vec<String>>, String> {
    // the working directory a `run:` step without its own gets
    let default_wd = get(job, "defaults")
        .and_then(|d| get(d, "run"))
        .and_then(|r| get_ci(r, "working-directory"))
        .map(py_str)
        .unwrap_or_else(|| wf_default_wd.to_string());
    fn norm(p: &str) -> String {
        p.split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect::<Vec<_>>()
            .join("/")
    }
    let mut out = Vec::new();
    if let Some(u) = get(job, "uses") {
        out.push(vec![py_str(u).trim().to_string()]);
    }
    // checkout path -> checked out from this repository?
    let mut paths: BTreeMap<String, bool> = BTreeMap::new();
    let mut unknown = false;
    let touches = Regex::new(r"\b(git|rm|mv|cp|tar|curl|wget|rsync|unzip)\b").unwrap();
    let drive = Regex::new(r"^([A-Za-z]:|//)").unwrap();
    let path_key = Regex::new(
        r"(?i)^(path|paths|dest|destination|dir|directory|target|working-directory|output|out|into|location|cwd|folder|root)$|-(dir|path|paths|directory|folder|root)$",
    )
    .unwrap();
    for st in list(get(job, "steps")) {
        let own = get(st, "uses").map(|u| py_str(u).trim().to_string());
        let action = own
            .as_deref()
            .map(|u| u.split('@').next().unwrap_or("").to_ascii_lowercase())
            .unwrap_or_default();
        if action == "actions/checkout" {
            let with = get(st, "with");
            // inputs spelled twice in different case: which one GitHub uses is not
            // modelled, so the checkout is not either
            let mut seen_keys = BTreeSet::new();
            if entries(with)
                .into_iter()
                .any(|(k, _)| !k.is_ascii() || !seen_keys.insert(k.to_ascii_lowercase()))
            {
                unknown = true;
            }
            // action inputs are matched without case, as GitHub does
            let path = with
                .and_then(|w| get_ci(w, "path"))
                .map(py_str)
                .unwrap_or_default();
            let repo = with.and_then(|w| get_ci(w, "repository")).map(py_str);
            if path.contains("${{") || path.contains('\\') || path.split('/').any(|c| c == "..") {
                unknown = true;
            }
            // another ref of this repository puts other action code there
            let pinned = with.and_then(|w| get_ci(w, "ref")).is_some();
            let same = !pinned
                && match repo.as_deref().map(str::trim) {
                    None => true,
                    Some(r) => {
                        let r: String = r.chars().filter(|c| !c.is_whitespace()).collect();
                        r == "${{github.repository}}"
                    }
                };
            paths.insert(norm(&path), same);
        } else if action.contains("checkout") {
            // a checkout sieve cannot model (a fork, a wrapper)
            unknown = true;
        }
        let mut seeds = Vec::new();
        collect_uses(st, &mut seeds);
        let mut step = Vec::new();
        for s in seeds {
            let Some(local) = s.strip_prefix("./") else {
                step.push(s);
                continue;
            };
            if unknown {
                return Err(format!(
                    "`uses: {s}` follows a checkout sieve cannot model (a path from an expression or with `..`, or a checkout action other than actions/checkout)"
                ));
            }
            if local.split('/').any(|c| c == "..") {
                return Err(format!("`uses: {s}` has a `..` segment"));
            }
            let rest = norm(local);
            let under = paths
                .iter()
                .filter(|(p, _)| {
                    !p.is_empty() && (rest == **p || rest.starts_with(&format!("{p}/")))
                })
                .max_by_key(|(p, _)| p.len());
            match under {
                Some((p, true)) => {
                    step.push(format!("./{}", rest[p.len()..].trim_start_matches('/')))
                }
                Some((p, false)) => {
                    return Err(format!(
                        "`uses: {s}` is under `{p}`, which a step checked out from another repository or ref, or rewrote"
                    ))
                }
                None if paths.get("") == Some(&false) => {
                    return Err(format!(
                        "`uses: {s}` is local, but another repository was checked out at the workspace root"
                    ))
                }
                None => step.push(s),
            }
        }
        // A later step that may rewrite a checked-out path makes it untrusted:
        // a `run:` that names it, or runs in it (its `working-directory:`, or
        // the job's or workflow's `defaults.run.working-directory`), with a
        // file or git command; any other action with an input pointing at,
        // under or above it. A path-like value sieve cannot place (`..`, an
        // expression, an absolute path) counts as touching every path. (The
        // workspace root is not tracked this way: a step that rewrites the
        // root is a limit for every local `uses:`.)
        let unplaceable = |v: &str| {
            let v = v.trim().replace('\\', "/");
            v.contains("${")
                || v.split('/').any(|c| c == "..")
                || v.starts_with('/')
                || drive.is_match(&v)
                || (v.starts_with('~') && !v.starts_with("~/."))
        };
        // `v` (one path per line) at or under `p`; for a path-like input also
        // above it (`.` or a parent replaces it too)
        let hits = |v: &str, p: &str, pathlike: bool| {
            v.lines().filter(|l| !l.trim().is_empty()).any(|l| {
                // `\` separates on Windows runners; macOS and Windows paths
                // ignore case
                let l = norm(
                    &l.trim()
                        .replace('\\', "/")
                        .trim_start_matches("$GITHUB_WORKSPACE")
                        .trim_start_matches('/')
                        .to_lowercase(),
                );
                let p = p.to_lowercase();
                let p = p.as_str();
                l == p
                    || l.starts_with(&format!("{p}/"))
                    || (pathlike && (l.is_empty() || p.starts_with(&format!("{l}/"))))
            })
        };
        if action != "actions/checkout" {
            if let Some(run) = get(st, "run").map(py_str) {
                let wd = get_ci(st, "working-directory")
                    .map(py_str)
                    .unwrap_or_else(|| default_wd.to_string());
                if touches.is_match(&run) {
                    let blind = !wd.is_empty() && unplaceable(&wd);
                    for (p, same) in paths.iter_mut() {
                        if !p.is_empty()
                            && (blind
                                || run
                                    .to_lowercase()
                                    .replace('\\', "/")
                                    .contains(&p.to_lowercase())
                                || hits(&wd, p, true))
                        {
                            *same = false;
                        }
                    }
                }
            }
            if !action.is_empty() {
                for (k, v) in entries(get(st, "with")) {
                    let v = py_str(v);
                    let pathlike = path_key.is_match(&k.replace('_', "-"));
                    let blind = pathlike && v.lines().any(unplaceable);
                    for (p, same) in paths.iter_mut() {
                        if !p.is_empty() && (blind || hits(&v, p, pathlike)) {
                            *same = false;
                        }
                    }
                }
            }
        }
        out.push(step);
    }
    Ok(out)
}

fn step_if_reads_mutable(cond: &str) -> bool {
    Regex::new(r"(?i)\b(needs|steps|env|jobs|matrix|strategy|inputs|vars|secrets)\b")
        .unwrap()
        .is_match(cond)
}

/// Security review of sieve#27: the classes read by classifier-gated jobs
/// that run a whole-repository linter (actionlint, yamllint, zizmor,
/// pre-commit, ...) in any of `workflows` (path, text). Such a job reads every
/// file under .github/, so a scoped .github/ edit selects its classes too. A
/// linter called from a script is not seen; its class must claim .github/.
pub fn linter_classes(
    workflows: &[(String, String)],
    actions: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>, String> {
    let lint = Regex::new(
        r"(?i)\b(actionlint|yamllint|yaml-lint|zizmor|pre-commit|prek|super-linter|megalinter|mega-linter|checkov|prettier|typos|codespell|reuse|markdownlint|markdownlint-cli2|lychee|check-jsonschema|editorconfig-checker|biome|dprint|taplo)\b",
    )
    .unwrap();
    let wf_by_path: BTreeMap<&str, &str> = workflows
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    // A local `uses:` path, normalized (`.` and `..` folded); `None` above the root.
    fn norm_local(p: &str) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        for c in p.split('/') {
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
    // The text of every local action or reusable workflow `seeds` name,
    // recursively; a local path that resolves to no tracked file, or an
    // unparseable one, fails. A cycle is followed once (`seen`).
    fn expand(
        seeds: Vec<String>,
        actions: &BTreeMap<String, String>,
        wfs: &BTreeMap<&str, &str>,
        seen: &mut BTreeSet<String>,
    ) -> Result<String, String> {
        let mut out = String::new();
        for u in seeds {
            let Some(raw) = u.strip_prefix("./") else {
                continue;
            };
            let raw = raw.split('@').next().unwrap_or("");
            let key =
                norm_local(raw).ok_or_else(|| format!("`uses: ./{raw}` leaves the repository"))?;
            if !seen.insert(key.clone()) {
                continue;
            }
            let inner = actions
                .get(&key)
                .map(String::as_str)
                .or_else(|| wfs.get(key.as_str()).copied())
                .ok_or_else(|| {
                    format!("`uses: ./{raw}` names no tracked action or workflow, so its linters cannot be found")
                })?;
            let y = rivet_yaml::from_str::<Yaml>(inner)
                .map_err(|e| format!("`uses: ./{raw}` is unparseable ({e})"))?;
            let mut nested = Vec::new();
            collect_uses(&y, &mut nested);
            out.push('\n');
            out.push_str(inner);
            out.push_str(&expand(nested, actions, wfs, seen)?);
        }
        Ok(out)
    }
    let mut out = BTreeSet::new();
    for (path, text) in workflows {
        let wf = rivet_yaml::from_str::<Yaml>(text)
            .map_err(|e| format!("{path} is unparseable ({e}), so its linters cannot be found"))?;
        let wf_default_wd = get(&wf, "defaults")
            .and_then(|d| get(d, "run"))
            .and_then(|r| get_ci(r, "working-directory"))
            .map(py_str)
            .unwrap_or_default();
        let jobs: BTreeMap<String, &Yaml> = entries(get(&wf, "jobs")).into_iter().collect();
        for (jid, job) in &jobs {
            let mut own = String::new();
            flatten(job, &mut own);
            // sieve#36: local `uses:` resolved through the job's checkout paths
            let per = checkout_seeds(job, &wf_default_wd)
                .map_err(|e| format!("{path}: job {jid}: {e}"))?;
            let full = own + &expand(per.concat(), actions, &wf_by_path, &mut BTreeSet::new())?;
            let per_step: &[Vec<String>] = if has(job, "uses") {
                &per[1..]
            } else {
                &per[..]
            };
            if !lint.is_match(&full) {
                continue;
            }
            // A gate inside the linter job (a step `if:` on its linter step,
            // or `needs` read outside `needs:`/`if:`) cannot be mapped to a
            // class: run everything.
            for (st, seeds) in list(get(job, "steps")).into_iter().zip(per_step) {
                let mut t = String::new();
                flatten(st, &mut t);
                let t = t + &expand(seeds.clone(), actions, &wf_by_path, &mut BTreeSet::new())?;
                if lint.is_match(&t)
                    && get(st, "if")
                        .is_some_and(|c| !unconditional_if(c) && step_if_reads_mutable(&py_str(c)))
                {
                    return Err(format!(
                        "{path}: job {jid} gates a whole-repository linter step with an `if:` sieve cannot map to a class"
                    ));
                }
            }
            // sieve#37: only `${{ }}` and `if:` can read a context
            if reads_needs(job, &["needs", "if"]) {
                return Err(format!(
                    "{path}: job {jid} runs a whole-repository linter and reads `needs` outside its `if:`"
                ));
            }
            // The linter job and every job it needs, transitively: a skipped
            // need skips it unless its own `if:` is always()/!cancelled().
            let mut stack = vec![jid.clone()];
            let mut seen = BTreeSet::new();
            while let Some(j) = stack.pop() {
                if !seen.insert(j.clone()) {
                    continue;
                }
                let Some(job) = jobs.get(&j) else { continue };
                match get(job, "if") {
                    None => {}
                    Some(c) if unconditional_if(c) => {}
                    Some(Yaml::String(c)) => match parse_gate_if(c) {
                        Ok((_, outs)) => out.extend(outs),
                        Err(_) => {
                            return Err(format!(
                                "{path}: job {j} gates a whole-repository linter with an `if:` that is not the classifier form"
                            ))
                        }
                    },
                    Some(_) => {
                        return Err(format!(
                            "{path}: job {j} gates a whole-repository linter with a non-string `if:`"
                        ))
                    }
                }
                let keeps_running = get(job, "if").is_some_and(unconditional_if);
                if keeps_running {
                    continue; // its needs cannot skip it
                }
                stack.extend(needs_of(job));
            }
        }
    }
    Ok(out)
}

/// The selection of a leaf-workflow edit, and a note when it came from the
/// file-wide rule (sieve#27 A) rather than from the changed jobs.
pub type LeafSelection = (BTreeMap<String, BTreeSet<String>>, Option<String>);

/// sieve#27 A: REQ-030 per-job scoping, with a file-wide fallback. `base` is
/// `None` for an added file; `others` are the repository's other workflow
/// files (path, text).
///
/// Everything runs, before any scoping, when either version of the file uses a
/// merge key, has no `on:`, is reusable (`workflow_call`), is triggered by
/// `workflow_run`, or another workflow's `workflow_run` names it.
///
/// When per-job scoping refuses (or the file is added), a file whose two
/// versions host no job running `sieve classify` and call no reusable
/// workflow selects nothing: none of its jobs can be gated by a classifier
/// (`needs:` stays inside a file), so every one of them runs anyway, and a
/// workflow file reaches another only through `workflow_call`, `workflow_run`
/// or a classifier. A file that hosts a classifier or calls a reusable
/// workflow keeps the refusal: everything runs.
pub fn leaf_scope_file(
    cfg: &Config,
    file: &str,
    base: Option<&str>,
    head: &str,
    others: &[(String, String)],
) -> Result<LeafSelection, String> {
    let parse = |t: &str, which: &str| -> Result<Yaml, String> {
        match rivet_yaml::from_str::<Yaml>(t) {
            Ok(y @ Yaml::Mapping(_)) => Ok(y),
            Ok(_) => Err(format!("{file} at {which} is not a mapping")),
            Err(e) => Err(format!("{file} at {which} is unparseable ({e})")),
        }
    };
    let h = parse(head, "head")?;
    let b = base.map(|t| parse(t, "the merge base")).transpose()?;
    let versions: Vec<&Yaml> = std::iter::once(&h).chain(b.as_ref()).collect();
    let mut names: BTreeSet<String> = [file.to_string(), basename(file).to_string()].into();
    for y in &versions {
        if has_merge_key(y) {
            return Err(format!("{file} uses a YAML merge key (`<<:`)"));
        }
        let triggers: Vec<String> = match on_of(y) {
            Some(Yaml::Mapping(m)) => m.iter().map(|(k, _)| py_str(k)).collect(),
            Some(other) => members(other),
            None => return Err(format!("{file} has no `on:`")),
        };
        if triggers.iter().any(|t| t == "workflow_call") {
            return Err(format!(
                "{file} is a reusable workflow (`on: workflow_call`)"
            ));
        }
        if triggers.iter().any(|t| t == "workflow_run") {
            return Err(format!("{file} is triggered by `workflow_run`"));
        }
        if let Some(n) = get_nn(y, "name") {
            names.insert(py_str(n));
        }
    }
    for (path, text) in others {
        let Ok(o) = rivet_yaml::from_str::<Yaml>(text) else {
            return Err(format!(
                "{path} is unparseable, so a `workflow_run` naming {file} cannot be ruled out"
            ));
        };
        if let Some(wr) = on_of(&o).and_then(|on| get(on, "workflow_run")) {
            let listed = get_nn(wr, "workflows").map(members).unwrap_or_default();
            if listed.is_empty() || listed.iter().any(|w| names.contains(w)) {
                return Err(format!("{path} runs on `workflow_run` of {file}"));
            }
        }
    }
    let refusal = match base {
        Some(bt) => match leaf_scope(cfg, file, bt, head) {
            Ok(sel) => return Ok((sel, None)),
            Err(why) => why,
        },
        None => format!("{file} is added"),
    };
    for y in &versions {
        if !classifier_jobs(y).is_empty() {
            return Err(format!("{refusal} (and the file hosts a classifier job)"));
        }
        if let Some((j, _)) = entries(get(y, "jobs"))
            .into_iter()
            .find(|(_, job)| has(job, "uses"))
        {
            return Err(format!("{refusal} (and job {j} calls a reusable workflow)"));
        }
        // Security review: a classifier sieve does not recognise (a wrapper
        // script, an action) still gates jobs. Its outputs reach a job only
        // through `needs`, read in `if:`, `env:`, `with:`, `strategy:` or a
        // step. So the file-wide rule does not apply when any job has an
        // `if:` beyond always()/!cancelled(), any step `if:` reads a context
        // other than github or runner, or any job reads `needs` anywhere
        // (its `needs:` list aside).
        for (j, job) in entries(get(y, "jobs")) {
            if get(job, "if").is_some_and(|c| !unconditional_if(c)) {
                return Err(format!(
                    "{refusal} (and job {j} has an `if:`, which may gate it)"
                ));
            }
            if list(get(job, "steps"))
                .into_iter()
                .any(|st| get(st, "if").is_some_and(|c| step_if_reads_mutable(&py_str(c))))
            {
                return Err(format!(
                    "{refusal} (and job {j} has a step `if:` reading a context other than github or runner)"
                ));
            }
            // sieve#37: only `${{ }}` and `if:` can read a context
            if reads_needs(job, &["needs"]) {
                return Err(format!("{refusal} (and job {j} reads `needs`)"));
            }
            // A job that needs another can receive a gate out of band (an
            // artifact or cache the other job wrote), which no text rule sees.
            if !needs_of(job).is_empty() {
                return Err(format!(
                    "{refusal} (and job {j} needs another job, which can pass it a gate as an artifact)"
                ));
            }
        }
    }
    Ok((
        BTreeMap::new(),
        Some(format!(
            "{refusal}; file-wide: the file hosts no classifier and calls no reusable workflow, so its jobs run anyway and no class is selected"
        )),
    ))
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// REQ-030: the classes a modification of the leaf workflow `file` selects,
/// as class -> the jobs that select it, given its text at the merge base and
/// at head. `Err` says why the change still selects everything.
///
/// Scoped only when the edit cannot change what is skipped: everything
/// outside `jobs:` is unchanged (triggers, env, permissions, concurrency,
/// defaults), the workflow is not reusable, no job is removed, every
/// job that runs `sieve classify` is unchanged, and no changed job changes a
/// gating key or a step's `if:`/`continue-on-error:`. The changed and added jobs, every
/// job that needs one (transitively), and every job those need must each be
/// ungated, `always()`/`!cancelled()`, or carry the exact classifier `if:`
/// wired to a classifier job in this file; the outputs they read are the
/// selected classes. A job among the changed ones or their dependents whose
/// steps are gated on job or step outputs selects everything.
pub fn leaf_scope(
    cfg: &Config,
    file: &str,
    base: &str,
    head: &str,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let parse = |t: &str, which: &str| -> Result<Yaml, String> {
        match rivet_yaml::from_str::<Yaml>(t) {
            Ok(y @ Yaml::Mapping(_)) => Ok(y),
            Ok(_) => Err(format!("{file} at {which} is not a mapping")),
            Err(e) => Err(format!("{file} at {which} is unparseable ({e})")),
        }
    };
    let (b, h) = (parse(base, "the merge base")?, parse(head, "head")?);
    let outside = |y: &Yaml| -> Vec<(Yaml, Yaml)> {
        match y {
            Yaml::Mapping(m) => m
                .iter()
                .filter(|(k, _)| !key_is(k, "jobs"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            _ => vec![],
        }
    };
    if outside(&b) != outside(&h) {
        return Err(format!(
            "{file} changed outside `jobs:` (triggers, env, permissions, concurrency or defaults)"
        ));
    }
    if has_merge_key(&b) || has_merge_key(&h) {
        return Err(format!("{file} uses a YAML merge key (`<<:`)"));
    }
    let triggers: Vec<String> = match on_of(&h) {
        Some(Yaml::Mapping(m)) => m.iter().map(|(k, _)| py_str(k)).collect(),
        Some(other) => members(other),
        None => return Err(format!("{file} has no `on:`")),
    };
    if triggers.iter().any(|t| t == "workflow_call") {
        return Err(format!(
            "{file} is a reusable workflow (`on: workflow_call`)"
        ));
    }
    let (Some(Yaml::Mapping(_)), Some(Yaml::Mapping(_))) = (get(&b, "jobs"), get(&h, "jobs"))
    else {
        return Err(format!("{file} has no `jobs:` mapping"));
    };
    let bmap: BTreeMap<String, &Yaml> = entries(get(&b, "jobs")).into_iter().collect();
    let hmap: BTreeMap<String, &Yaml> = entries(get(&h, "jobs")).into_iter().collect();
    let removed: Vec<&String> = bmap.keys().filter(|k| !hmap.contains_key(*k)).collect();
    if !removed.is_empty() {
        return Err(format!("{file} removes jobs {removed:?}"));
    }
    let cls = classifier_jobs(&h);
    let bcls = classifier_jobs(&b);
    if cls.keys().ne(bcls.keys()) {
        return Err(format!("{file} changed which jobs run `sieve classify`"));
    }
    // GitHub matches `needs:` case-insensitively; the closures below match
    // exactly, so an entry that is not exactly a job id selects everything.
    for m in [&bmap, &hmap] {
        for (jid, job) in m {
            if let Some(n) = needs_of(job).into_iter().find(|n| !m.contains_key(n)) {
                return Err(format!(
                    "{file}: job {jid} needs `{n}`, which is not exactly a job id"
                ));
            }
        }
    }
    let mut changed: BTreeSet<String> = BTreeSet::new();
    for (jid, hjob) in &hmap {
        // An added job is a changed job with nothing to compare: no job in
        // the base can need it, and its own gate decides its classes below.
        let Some(&bjob) = bmap.get(jid) else {
            changed.insert(jid.clone());
            continue;
        };
        if bjob == *hjob {
            continue;
        }
        if cls.contains_key(jid) {
            return Err(format!(
                "{file}: job {jid} runs `sieve classify` and changed (classifier wiring)"
            ));
        }
        for k in GATING_KEYS {
            if get(bjob, k) != get(hjob, k) {
                return Err(format!("{file}: job {jid} changed its `{k}:`"));
            }
        }
        if step_gates(bjob) != step_gates(hjob) {
            return Err(format!(
                "{file}: job {jid} changed a step's `if:` or `continue-on-error:`"
            ));
        }
        changed.insert(jid.clone());
    }
    if changed.is_empty() {
        return Err(format!(
            "{file}: no job changed as parsed (a comment or layout edit is not scoped)"
        ));
    }
    // A job that needs a changed job reads its result or outputs.
    let mut affected = changed.clone();
    loop {
        let grown: Vec<String> = hmap
            .iter()
            .filter(|(j, job)| {
                !affected.contains(*j) && needs_of(job).iter().any(|n| affected.contains(n))
            })
            .map(|(j, _)| j.clone())
            .collect();
        if grown.is_empty() {
            break;
        }
        affected.extend(grown);
    }
    for j in &affected {
        if cls.contains_key(j) {
            return Err(format!("{file}: classifier job {j} needs a changed job"));
        }
        // A matrix computed from an expression (fromJSON(needs.x.outputs.m))
        // can come out empty, and an empty matrix reports the job skipped.
        let mut strategy = String::new();
        if let Some(s) = get(hmap[j], "strategy") {
            flatten(s, &mut strategy);
        }
        if strategy.contains("${{") {
            return Err(format!(
                "{file}: job {j} has a strategy computed from an expression"
            ));
        }
        for st in list(get(hmap[j], "steps")) {
            if step_if_reads_mutable(&get(st, "if").map(py_str).unwrap_or_default()) {
                return Err(format!(
                    "{file}: job {j} has a step gated on a context other than github or runner"
                ));
            }
        }
        // Security review of sieve#27: a job that reads `needs` outside its
        // `if:` and `needs:` (env, with, a run script) can skip its own work
        // on a classifier output, e.g. `[ "$A" = false ] && exit 0`.
        // sieve#37: only `${{ }}` and `if:` can read a context
        if reads_needs(hmap[j], &["needs", "if"]) {
            return Err(format!(
                "{file}: job {j} reads `needs` outside its `if:`, so its work may depend on a classifier output"
            ));
        }
    }
    // A skipped need skips the job that needs it.
    let mut closure = affected.clone();
    let mut stack: Vec<String> = affected.iter().cloned().collect();
    while let Some(j) = stack.pop() {
        for n in needs_of(hmap[&j]) {
            if !hmap.contains_key(&n) {
                return Err(format!("{file}: job {j} needs `{n}`, which is not a job"));
            }
            if closure.insert(n.clone()) {
                stack.push(n);
            }
        }
    }
    let names: BTreeSet<&str> = cfg.classes.iter().map(|c| c.name.as_str()).collect();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for j in &closure {
        let job = hmap[j];
        if cls.contains_key(j) {
            continue; // unchanged, ungated (classifier_wiring rejects an if:)
        }
        let Some(cond) = get_nn(job, "if") else {
            continue; // runs whenever the workflow runs
        };
        if unconditional_if(cond) {
            continue;
        }
        let Yaml::String(cond) = cond else {
            return Err(format!("{file}: job {j} has a non-string `if:`"));
        };
        let (c, outs) = parse_gate_if(cond).map_err(|why| format!("{file}: job {j}: {why}"))?;
        let Some((_, wired)) = cls.get(&c) else {
            return Err(format!(
                "{file}: job {j} reads needs.{c}, which does not run `sieve classify`"
            ));
        };
        if !needs_of(job).contains(&c) {
            return Err(format!(
                "{file}: job {j} reads needs.{c} but does not need it"
            ));
        }
        let probs = classifier_wiring(&h, &c, &cfg.path);
        if !probs.is_empty() {
            return Err(format!("{file}: {}", probs.join("; ")));
        }
        for o in outs {
            if !wired.contains(&o) {
                return Err(format!(
                    "{file}: job {j} reads output {o:?}, which job {c} does not wire to the classifier step"
                ));
            }
            if !names.contains(o.as_str()) {
                return Err(format!(
                    "{file}: job {j} reads output {o:?}, which is not a class"
                ));
            }
            out.entry(o).or_default().insert(j.clone());
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- report

#[derive(Default)]
struct Report {
    n: usize,
    failures: Vec<serde_json::Value>,
    refused: Vec<serde_json::Value>,
    ok: Vec<(String, String)>,
    text: Vec<String>,
}

impl Report {
    #[allow(clippy::too_many_arguments)]
    fn fail(&mut self, gate: &str, ctx: &str, wf: &str, job: &str, code: &str, msg: String) {
        self.text.push(format!(
            "  FAIL [{gate}] {code:<22} {ctx}   [{wf}] -- {msg}"
        ));
        self.failures.push(json!({
            "gate": gate, "context": ctx, "workflow": wf, "job": job,
            "code": code, "message": msg,
        }));
    }
    fn refuse(&mut self, input: &str, msg: impl Into<String>) {
        self.refused
            .push(json!({"input": input, "message": msg.into()}));
    }
}

pub fn cmd_check_workflows(
    workflows: Option<&str>,
    required: Option<&str>,
    format: Option<&str>,
    root: PathBuf,
    cfg: &Config,
) -> ExitCode {
    let format = format.unwrap_or("text");
    if !["json", "text"].contains(&format) {
        eprintln!("sieve: unknown --format `{format}`");
        return ExitCode::from(2);
    }
    let at = |p: &str| -> PathBuf {
        if Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            root.join(p)
        }
    };
    let wfdir = at(workflows.unwrap_or(DEFAULT_WORKFLOWS));
    let req_rel = required
        .or(cfg.workflows.required_contexts.as_deref())
        .unwrap_or(DEFAULT_REQUIRED);
    let reqfile = at(req_rel);
    let mut r = Report::default();

    let contexts: Vec<String> = match std::fs::read_to_string(&reqfile) {
        Ok(t) => t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(e) => {
            r.refuse(
                &reqfile.to_string_lossy(),
                format!("required contexts: {e}"),
            );
            vec![]
        }
    };
    let mut files: Vec<(String, PathBuf)> = vec![];
    match std::fs::read_dir(&wfdir) {
        Ok(rd) => {
            let name_re = Regex::new(r"^.*\.y.*ml$").unwrap();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if name_re.is_match(&n) && e.path().is_file() {
                    files.push((n, e.path()));
                }
            }
            files.sort();
        }
        Err(e) => r.refuse(
            &wfdir.to_string_lossy(),
            format!("workflows directory: {e}"),
        ),
    }
    if !r.refused.is_empty() {
        return finish(r, format, &contexts, &wfdir);
    }

    let mut wfs: Vec<Wf> = vec![];
    for (name, path) in &files {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| rivet_yaml::from_str::<Yaml>(&t).map_err(|e| e.to_string()));
        r.n += 1;
        match parsed {
            Err(e) => {
                // The reference skips an unparseable file; a file whose jobs
                // cannot be read is a gate that cannot run, so it fails here.
                r.fail(
                    "a",
                    "",
                    name,
                    "",
                    "unparseable",
                    format!("unparseable ({e})"),
                );
            }
            Ok(doc) => {
                if matches!(doc, Yaml::Mapping(_)) {
                    let state = trigger_state(&doc);
                    wfs.push(Wf {
                        file: name.clone(),
                        doc,
                        state,
                    });
                }
            }
        }
    }
    // job display name -> hits
    let mut idx: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
    for (wi, wf) in wfs.iter().enumerate() {
        for (jid, job) in entries(get(&wf.doc, "jobs")) {
            // sieve#30: a name built from `${{ matrix.<key> }}` is expanded per row
            for name in matrix_names(job, &jid) {
                idx.entry(name).or_default().push(Hit {
                    wf: wi,
                    jid: jid.clone(),
                });
            }
        }
    }
    let matrix_suffix = Regex::new(r"\s*\(.*\)\s*$").unwrap();
    let lookup = |ctx: &str| -> Vec<&Hit> {
        idx.get(ctx)
            .or_else(|| idx.get(matrix_suffix.replace(ctx, "").as_ref()))
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    };

    // Job ids, per workflow file, that host a required context: a need on one
    // of these is visible, because if it fails its own required check is red.
    let mut required_jids: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for ctx in &contexts {
        for h in lookup(ctx) {
            required_jids.entry(h.wf).or_default().insert(h.jid.clone());
        }
    }
    let mut classifier_outputs: BTreeSet<String> = BTreeSet::new();
    let mut classifier_where: BTreeSet<String> = BTreeSet::new();
    let none = BTreeSet::new();
    for ctx in &contexts {
        let hits = lookup(ctx);
        r.n += 1;
        if hits.is_empty() {
            r.fail(
                "a",
                ctx,
                "",
                ctx,
                "unmapped",
                "no job in any workflow produces this name".into(),
            );
            continue;
        }
        for h in hits {
            let wf = &wfs[h.wf];
            let mut problem = wf.state.clone().map(|(c, d)| ("a", c, d));
            if problem.is_none() {
                let s = job_skip_state(cfg, wf, &h.jid, required_jids.get(&h.wf).unwrap_or(&none));
                for (c, o) in s.used {
                    classifier_outputs.insert(o);
                    classifier_where.insert(format!("{}:{c}", wf.file));
                }
                problem = s.problem.map(|(c, d)| ("b", c, d));
            }
            match problem {
                None => {
                    r.text
                        .push(format!("    ok             {ctx}   [{}]", wf.file));
                    r.ok.push((ctx.clone(), wf.file.clone()));
                }
                Some((gate, code, detail)) => r.fail(gate, ctx, &wf.file, &h.jid, code, detail),
            }
        }
    }

    // never-skip guard jobs (REQ-028): each runs on every pull request and in
    // the merge queue, and does its work. Fail closed on anything that could
    // skip it or empty it: an if: beyond always()/!cancelled() on the job, on
    // any job it needs (transitively), or on any of its steps; a missing need;
    // a truthy continue-on-error; a matrix given as an expression or with an
    // empty axis; a reusable-workflow call (its callee is not checked here);
    // a workflow trigger gate (a) rejects.
    for name in &cfg.workflows.never_skip_jobs {
        r.n += 1;
        let mut found = false;
        for wf in &wfs {
            let jobs = get(&wf.doc, "jobs");
            for (jid, job) in entries(jobs) {
                if job_name(job, &jid) != *name && jid != *name {
                    continue;
                }
                found = true;
                let ctx = format!("never-skip {name}");
                match never_skip_problem(&wf.state, jobs, &jid, job) {
                    Some((code, detail)) => r.fail(
                        if code.starts_with("never-skip-trigger") {
                            "a"
                        } else {
                            "b"
                        },
                        &ctx,
                        &wf.file,
                        &jid,
                        &code,
                        detail,
                    ),
                    None => r
                        .text
                        .push(format!("    never-skip     {name}   [{}]", wf.file)),
                }
            }
        }
        if !found {
            r.fail(
                "b",
                &format!("never-skip {name}"),
                "",
                name,
                "never-skip-missing",
                "no job with this name or id in any workflow".into(),
            );
        }
    }

    // (d) REQ-026: a gated job that builds a Cargo package by name or manifest
    // must have a class that claims that package's manifest; otherwise a
    // change to that package skips a job that builds it.
    let builds = cargo_builds(&wfs, cfg);
    if !builds.is_empty() {
        cross_class_check(&mut r, &builds, &root, cfg);
    }

    // (c) the classifier it reads fails OPEN
    if !classifier_outputs.is_empty() {
        let where_ = classifier_where
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if !root.join(".git").exists() {
            r.refuse("c", "no .git: the fail-open spot checks need git");
        } else {
            let cases = crate::check::fail_open_cases(&root, cfg);
            let n = cases.len();
            let mut held = true;
            for (ok, msg) in cases {
                r.n += 1;
                if !ok {
                    held = false;
                    r.fail(
                        "c",
                        "",
                        "",
                        "(classifier)",
                        "fails-closed",
                        format!("{msg} [{where_}]"),
                    );
                }
            }
            if held {
                r.text.push(format!(
                    "    fails open     sieve classify: {n} scenario(s) x {} class(es), every output of {} read is 'true'  [{where_}]",
                    cfg.classes.len(),
                    classifier_outputs.len()
                ));
            }
        }
    }
    finish(r, format, &contexts, &wfdir)
}

fn finish(r: Report, format: &str, contexts: &[String], wfdir: &Path) -> ExitCode {
    if format == "json" {
        let out = json!({
            "schema": crate::WORKFLOWS_SCHEMA,
            "pass": r.failures.is_empty() && r.refused.is_empty(),
            "checks": r.n,
            "failures": r.failures,
            "refused": r.refused,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        println!(
            "  {} required context(s) against {}\n",
            contexts.len(),
            wfdir.display()
        );
        for l in &r.text {
            println!("{l}");
        }
        for f in &r.refused {
            println!(
                "  REFUSED [{}] {}",
                f["input"].as_str().unwrap_or(""),
                f["message"].as_str().unwrap_or("")
            );
        }
        println!(
            "\n    UNGUARDED: that this file still matches the real branch protection was NOT\n    checked (reading protection needs admin rights the default GITHUB_TOKEN lacks).\n    The [workflows.job_outputs] map is HAND-MAINTAINED: this check proves a job reads\n    only its own outputs, not that the map is right (the false-skip controls test that).\n"
        );
        if !r.failures.is_empty() {
            println!(
                "  FAIL: {} problem(s): a required context may not be produced, or may be skipped by something other than the change classifier",
                r.failures.len()
            );
        } else if !r.refused.is_empty() {
            println!("  REFUSED: {} input(s) missing", r.refused.len());
        } else {
            println!(
                "  PASS: {} required context(s) map to jobs that report on every PR and in the merge queue,\n        and can be skipped only through the classifier wiring this check verifies",
                contexts.len()
            );
        }
    }
    if !r.failures.is_empty() {
        ExitCode::from(1)
    } else if !r.refused.is_empty() {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}
