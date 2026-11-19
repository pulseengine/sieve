//! `sieve check-graph`: controls on the real tree. Exit 0 pass, 1 a control
//! failed, 2 usage, 3 a control could not run (refused).

use crate::classify::{self, is_run_all};
use crate::config::Config;
use crate::graph::{self, Graph};
use crate::repo::{norm, Repo};
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

pub const CONTROLS: &[&str] = &[
    "graph-inputs",
    "cargo-metadata",
    "true-inputs",
    "no-job",
    "inert-overlap",
    "fail-open",
    "bazel-srcs",
    "bazel-snapshot",
    "depinfo",
];

#[derive(Default)]
struct Report {
    n: usize,
    failures: Vec<serde_json::Value>,
    refused: Vec<serde_json::Value>,
}

impl Report {
    fn expect(&mut self, control: &str, ok: bool, code: &str, msg: impl FnOnce() -> String) {
        self.n += 1;
        if !ok {
            self.failures
                .push(json!({"control": control, "code": code, "message": msg()}));
        }
    }
    fn refuse(&mut self, control: &str, msg: impl Into<String>) {
        self.refused
            .push(json!({"control": control, "message": msg.into()}));
    }
}

pub fn cmd_check_graph(
    controls: &[String],
    depinfo: &[String],
    format: Option<&str>,
    root: PathBuf,
    cfg: &Config,
) -> ExitCode {
    let format = format.unwrap_or("text");
    if !["json", "text"].contains(&format) {
        eprintln!("sieve: unknown --format `{format}`");
        return ExitCode::from(2);
    }
    for c in controls {
        if !CONTROLS.contains(&c.as_str()) {
            eprintln!(
                "sieve: unknown control `{c}` (known: {})",
                CONTROLS.join(", ")
            );
            return ExitCode::from(2);
        }
    }
    let mut dep_specs = Vec::new();
    for d in depinfo {
        let Some((class, file)) = d.split_once('=') else {
            eprintln!("sieve: --depinfo takes <class>=<file.d>");
            return ExitCode::from(2);
        };
        if !cfg.classes.iter().any(|c| c.name == class) {
            eprintln!("sieve: --depinfo: no class `{class}`");
            return ExitCode::from(2);
        }
        dep_specs.push((class.to_string(), file.to_string()));
    }
    let selected: Vec<&str> = if controls.is_empty() {
        CONTROLS
            .iter()
            .copied()
            .filter(|c| *c != "depinfo" || !dep_specs.is_empty())
            .collect()
    } else {
        controls.iter().map(String::as_str).collect()
    };
    let has_git = root.join(".git").exists();
    let repo = Repo {
        root: root.clone(),
        git: has_git,
    };
    let mut r = Report::default();

    let needs_graph = selected
        .iter()
        .any(|c| !["bazel-srcs", "fail-open"].contains(c));
    let g: Option<Graph> = if needs_graph {
        match graph::derive(&repo, cfg) {
            Ok(g) => Some(g),
            Err(e) => {
                r.expect("graph", false, e.code, || {
                    format!("the build graph cannot be derived: {}", e.message)
                });
                None
            }
        }
    } else {
        None
    };
    let tracked = repo.tracked().unwrap_or_default();

    for &c in &selected {
        match (c, &g) {
            ("bazel-srcs", _) => {
                for class in &cfg.classes {
                    for b in &class.bazel_build {
                        match graph::bazel_literal_srcs(&repo, b) {
                            Ok(srcs) => {
                                for s in srcs {
                                    r.expect(c, repo.exists(&s), "bazel-src-missing", || {
                                        format!("{b}: src {s} does not exist")
                                    });
                                }
                            }
                            Err(e) => r.expect(c, false, e.code, || e.message.clone()),
                        }
                    }
                }
            }
            ("fail-open", _) => {
                if !has_git {
                    r.refuse(c, "no .git: the fail-open spot checks need git");
                    continue;
                }
                for (ok, msg) in fail_open_cases(&root, cfg) {
                    r.expect(c, ok, "fail-open", || msg);
                }
            }
            (_, None) => {} // graph failure already reported
            ("graph-inputs", Some(g)) => {
                for (name, cg) in g {
                    r.expect(c, cg.unresolved.is_empty(), "unresolved", || {
                        format!("{name}: unresolved input(s) {:?}: the class always runs; resolve or record", &cg.unresolved[..cg.unresolved.len().min(3)])
                    });
                    let files: Vec<&String> = tracked
                        .iter()
                        .filter(|f| cg.claims_path(f) && !is_run_all(cfg, f))
                        .collect();
                    r.expect(c, !files.is_empty(), "claims-nothing", || {
                        format!("{name}: the graph claims no tracked file")
                    });
                    for f in files {
                        let v = classify::classify(&repo, cfg, std::slice::from_ref(f), g);
                        let ok = matches!(&v, Ok(v) if v.classes.get(name) == Some(&true));
                        let docs =
                            matches!(&v, Ok(v) if !v.run_all && v.classes.values().all(|b| !b));
                        // A file the docs rule treats as docs only selects nothing, by design.
                        r.expect(c, ok || docs, "input-not-selecting", || {
                            format!("{name}: graph input {f} does not select it")
                        });
                    }
                }
            }
            ("cargo-metadata", Some(g)) => cargo_metadata(&repo, cfg, g, &mut r),
            ("true-inputs", Some(g)) => {
                for class in &cfg.classes {
                    for p in &class.true_inputs {
                        r.expect(c, tracked.contains(p), "true-input-untracked", || {
                            format!("{}: true input {p} is not a tracked file", class.name)
                        });
                        let v = classify::classify(&repo, cfg, std::slice::from_ref(p), g);
                        r.expect(
                            c,
                            matches!(&v, Ok(v) if v.classes.get(&class.name) == Some(&true)),
                            "false-skip",
                            || {
                                format!(
                                    "{p} is a true input of {} but does not select it",
                                    class.name
                                )
                            },
                        );
                        // ...and selects it because the graph claims it, not as an unknown path.
                        r.expect(c, g[&class.name].claims_path(p), "edge-lost", || {
                            format!("{p} selects {} only as an unknown path; the graph walk lost this edge", class.name)
                        });
                    }
                }
            }
            ("no-job", Some(g)) => {
                for p in &cfg.no_job {
                    r.expect(c, tracked.contains(p), "no-job-untracked", || {
                        format!("no-job control {p} is not a tracked file")
                    });
                    let v = classify::classify(&repo, cfg, std::slice::from_ref(p), g);
                    r.expect(
                        c,
                        matches!(&v, Ok(v) if !v.run_all && v.classes.values().all(|b| !b)),
                        "selects-a-job",
                        || {
                            format!(
                                "{p} must select no class, got {:?}",
                                v.as_ref().map(|v| v
                                    .classes
                                    .iter()
                                    .filter(|x| *x.1)
                                    .map(|x| x.0.clone())
                                    .collect::<Vec<_>>())
                            )
                        },
                    );
                }
            }
            ("inert-overlap", Some(g)) => {
                for (name, cg) in g {
                    for claim in cg.claims.keys() {
                        let overlaps = classify::is_inert(cfg, claim)
                            || (claim.ends_with('/')
                                && cfg
                                    .inert_prefixes
                                    .iter()
                                    .any(|i| i.starts_with(claim.as_str())))
                            || (claim.ends_with('/')
                                && cfg
                                    .inert_files
                                    .iter()
                                    .any(|i| i.starts_with(claim.as_str())));
                        r.expect(c, !overlaps, "inert-overlap", || {
                            format!("{name}: graph input {claim} overlaps the inert list")
                        });
                    }
                }
            }
            ("bazel-snapshot", Some(g)) => {
                for class in &cfg.classes {
                    if class.bazel_snapshot.is_some() {
                        let stale = g[&class.name]
                            .unresolved
                            .iter()
                            .any(|u| u.code == "bazel-snapshot-stale");
                        r.expect(c, !stale, "bazel-snapshot-stale", || {
                            format!(
                                "{}: the Bazel snapshot is stale; regenerate and commit it",
                                class.name
                            )
                        });
                    }
                }
            }
            ("depinfo", Some(g)) => {
                if dep_specs.is_empty() {
                    r.refuse(c, "no --depinfo <class>=<file.d> given");
                }
                for (class, file) in &dep_specs {
                    let path = if PathBuf::from(file).is_absolute() {
                        PathBuf::from(file)
                    } else {
                        root.join(file)
                    };
                    let text = match std::fs::read_to_string(&path) {
                        Ok(t) => t,
                        Err(e) => {
                            r.refuse(c, format!("{file}: {e}"));
                            continue;
                        }
                    };
                    for p in depinfo_inputs(&root, &text) {
                        r.expect(c, is_run_all(cfg, &p) || g[class].claims_path(&p), "depinfo-unclaimed", || {
                            format!("{class}: rustc read {p} ({file}) but the graph does not claim it")
                        });
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    let pass = r.failures.is_empty() && r.refused.is_empty();
    if format == "json" {
        let out = json!({
            "schema": crate::CHECK_SCHEMA,
            "pass": pass,
            "controls": r.n,
            "failures": r.failures,
            "refused": r.refused,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for f in &r.failures {
            println!(
                "  FAIL [{}] {}: {}",
                f["control"].as_str().unwrap_or(""),
                f["code"].as_str().unwrap_or(""),
                f["message"].as_str().unwrap_or("")
            );
        }
        for f in &r.refused {
            println!(
                "  REFUSED [{}] {}",
                f["control"].as_str().unwrap_or(""),
                f["message"].as_str().unwrap_or("")
            );
        }
        println!(
            "  {}/{} control(s) hold; {} refused",
            r.n - r.failures.len(),
            r.n,
            r.refused.len()
        );
    }
    if !r.failures.is_empty() {
        ExitCode::from(1)
    } else if !r.refused.is_empty() {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// The fail-open spot checks, shared with `check-workflows` gate (c): push,
/// merge_group, a pull request with no base, a base git cannot diff and an
/// empty diff must each select every class. One `(held, message)` per case.
pub fn fail_open_cases(root: &std::path::Path, cfg: &Config) -> Vec<(bool, String)> {
    let zero = "0".repeat(40);
    let cases: [(Option<&str>, Option<&str>); 5] = [
        (Some("push"), None),
        (Some("merge_group"), Some("HEAD")),
        (Some("pull_request"), Some("HEAD")),
        (Some("pull_request"), Some(zero.as_str())),
        (Some("pull_request"), None),
    ];
    cases
        .into_iter()
        .map(|(ev, base)| {
            let v = classify::git_mode(root, cfg, ev, base, None);
            (
                v.run_all && v.classes.values().all(|b| *b),
                format!(
                    "event {ev:?} base {base:?}: expected everything selected, got {:?}",
                    v.reason
                ),
            )
        })
        .collect()
}

/// Repository-relative inputs named by a rustc dep-info file. Absolute paths
/// outside the repository (toolchain, registry) and build outputs under
/// target/ are not inputs to classify.
pub fn depinfo_inputs(root: &std::path::Path, text: &str) -> Vec<String> {
    let root_abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Some((_, deps)) = split_rule(line) else {
            continue;
        };
        for tok in split_escaped(deps) {
            let p = PathBuf::from(&tok);
            let rel = if p.is_absolute() {
                match p.strip_prefix(&root_abs).or_else(|_| p.strip_prefix(root)) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                }
            } else {
                match norm("", &tok) {
                    Some(r) => r,
                    None => continue,
                }
            };
            if rel.is_empty() || rel.starts_with("target/") {
                continue;
            }
            out.insert(rel);
        }
    }
    out.into_iter().collect()
}

/// `target: deps` with the colon not part of a Windows drive or an escape.
fn split_rule(line: &str) -> Option<(&str, &str)> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ' || b[i + 1] == b'\t') {
            return Some((&line[..i], &line[i + 1..]));
        }
        i += 1;
    }
    None
}

fn split_escaped(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' if it.peek() == Some(&' ') => {
                cur.push(' ');
                it.next();
            }
            ' ' | '\t' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// cargo itself agrees: every package and target `cargo metadata` reports,
/// following path dependencies, is claimed by the walk.
fn cargo_metadata(repo: &Repo, cfg: &Config, g: &Graph, r: &mut Report) {
    let root_abs = repo
        .root
        .canonicalize()
        .unwrap_or_else(|_| repo.root.clone());
    let rel = |p: &str| -> Option<String> {
        PathBuf::from(p)
            .strip_prefix(&root_abs)
            .ok()
            .map(|x| x.to_string_lossy().replace('\\', "/"))
    };
    for class in &cfg.classes {
        let mut manifests = class.cargo.clone();
        for pat in &class.cargo_glob {
            manifests.extend(repo.glob(pat));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut todo: Vec<(String, bool)> = manifests.into_iter().map(|m| (m, true)).collect();
        while let Some((m, is_root)) = todo.pop() {
            if !seen.insert((m.clone(), is_root)) {
                continue;
            }
            let o = Command::new("cargo")
                .args([
                    "metadata",
                    "--no-deps",
                    "--offline",
                    "--format-version",
                    "1",
                    "--manifest-path",
                ])
                .arg(repo.path(&m))
                .current_dir(&repo.root)
                .output();
            let o = match o {
                Ok(o) => o,
                Err(e) => {
                    r.refuse("cargo-metadata", format!("cargo: {e}"));
                    return;
                }
            };
            if !o.status.success() {
                r.expect("cargo-metadata", false, "cargo-metadata-failed", || {
                    format!(
                        "cargo metadata {m}: {}",
                        String::from_utf8_lossy(&o.stderr)
                            .trim()
                            .chars()
                            .take(300)
                            .collect::<String>()
                    )
                });
                continue;
            }
            let meta: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
            for pkg in meta["packages"].as_array().into_iter().flatten() {
                let Some(pm) = pkg["manifest_path"].as_str().and_then(rel) else {
                    continue;
                };
                if pm != m && !is_root {
                    continue;
                }
                r.expect("cargo-metadata", g[&class.name].claims_path(&pm), "cargo-unclaimed", || {
                    format!("{}: cargo metadata reports manifest {pm} but the walk does not claim it", class.name)
                });
                for t in pkg["targets"].as_array().into_iter().flatten() {
                    let kinds: Vec<&str> = t["kind"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|k| k.as_str())
                        .collect();
                    let compiled = is_root
                        || kinds.iter().any(|k| {
                            [
                                "lib",
                                "rlib",
                                "cdylib",
                                "staticlib",
                                "dylib",
                                "proc-macro",
                                "custom-build",
                            ]
                            .contains(k)
                        });
                    if !compiled {
                        continue;
                    }
                    let Some(src) = t["src_path"].as_str().and_then(rel) else {
                        continue;
                    };
                    r.expect("cargo-metadata", g[&class.name].claims_path(&src), "cargo-unclaimed", || {
                        format!("{}: cargo metadata reports target {} {src} (from {m}) but the walk does not claim it", class.name, kinds.join("/"))
                    });
                }
                for d in pkg["dependencies"].as_array().into_iter().flatten() {
                    if let Some(p) = d["path"].as_str().and_then(rel) {
                        if is_root || d["kind"].as_str() != Some("dev") {
                            let dm = if p.is_empty() {
                                "Cargo.toml".to_string()
                            } else {
                                format!("{p}/Cargo.toml")
                            };
                            todo.push((dm, false));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depinfo_parses_and_filters() {
        let root = std::env::temp_dir();
        let d = "target/x.d: ffi/src/lib.rs /rustup/core/lib.rs a\\ b.rs\n\nffi/src/lib.rs:\n";
        assert_eq!(
            depinfo_inputs(&root, d),
            vec!["a b.rs".to_string(), "ffi/src/lib.rs".to_string()]
        );
    }
}
