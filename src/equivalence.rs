//! `sieve equivalence`: the switch-over gate (REQ-013). Over the last N
//! first-parent commits of a ref, run the consumer's old detector in
//! GATE_PROBE_FILES mode on each commit's own diff and classify the same
//! paths with sieve; every class sieve selects less than the old detector is
//! a narrowing. Exit 0 no narrowing and no error, 1 a narrowing or an
//! uncompared commit (old detector failed), 2 usage, 3 refused (nothing
//! could be compared).

use crate::classify;
use crate::config::Config;
use crate::gitrepo;
use crate::repo::Repo;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub struct Opts {
    pub old: String,
    pub old_args: Vec<String>,
    pub refspec: String,
    pub commits: usize,
    /// old output name -> the sieve classes it stands for (one-to-many)
    pub map: BTreeMap<String, Vec<String>>,
    /// class -> path prefixes under which its narrowing is intended. A
    /// commit's narrowing of the class is accepted only if EVERY path the
    /// commit changed starts with one prefix; "" accepts the whole class.
    pub accept_narrowing: BTreeMap<String, Vec<String>>,
    pub format: String,
}

/// A fresh scratch directory nobody else can write into: created with
/// `create_dir` (fails if the name exists) under an unpredictable name, mode
/// 0700 on unix, so no file in it can be pre-planted or symlinked.
fn private_dir() -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    let base = std::env::temp_dir();
    for attempt in 0..16u32 {
        // std's RandomState draws fresh random keys: an unpredictable name.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(attempt);
        let d = base.join(format!("sieve-equivalence-{:016x}", h.finish()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700); // set at creation: no window with wider permissions
        }
        match b.create(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not create a private scratch directory",
    ))
}

/// Run the old detector on one path list; its outputs as name -> bool.
fn run_old(
    o: &Opts,
    root: &Path,
    paths_file: &Path,
    out_file: &Path,
) -> Result<BTreeMap<String, bool>, String> {
    let _ = std::fs::remove_file(out_file);
    let st = Command::new(&o.old)
        .args(&o.old_args)
        .current_dir(root)
        .env("GATE_PROBE_FILES", paths_file)
        .env("GITHUB_OUTPUT", out_file)
        .env_remove("EVENT_NAME")
        .env_remove("BASE_SHA")
        .output()
        .map_err(|e| format!("run {}: {e}", o.old))?;
    if !st.status.success() {
        return Err(format!("{} exited {:?}", o.old, st.status.code()));
    }
    let text = std::fs::read_to_string(out_file)
        .map_err(|e| format!("{} wrote no GITHUB_OUTPUT: {e}", o.old))?;
    let mut m = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let sel = v.trim() == "true";
            match o.map.get(k.trim()) {
                Some(classes) => {
                    for c in classes {
                        m.insert(c.clone(), sel);
                    }
                }
                None => {
                    m.insert(k.trim().to_string(), sel);
                }
            }
        }
    }
    Ok(m)
}

pub fn cmd(o: &Opts, root: PathBuf, cfg: &Config) -> ExitCode {
    let commits = match gitrepo::first_parent_commits(&root, &o.refspec, o.commits) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sieve equivalence: refused: {e}");
            return ExitCode::from(3);
        }
    };
    let tmp = match private_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sieve equivalence: refused: {e}");
            return ExitCode::from(3);
        }
    };
    let (paths_file, out_file) = (tmp.join("paths.txt"), tmp.join("github_output"));
    let repo = Repo {
        root: root.clone(),
        git: false,
    };
    let classes: BTreeSet<String> = cfg.classes.iter().map(|c| c.name.clone()).collect();
    let (mut identical, mut wider, mut errors) = (0usize, 0usize, Vec::new());
    let mut narrower: Vec<serde_json::Value> = Vec::new();
    let mut accepted: Vec<serde_json::Value> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    let mut compared_classes: BTreeSet<String> = BTreeSet::new();
    let mut compared = 0usize;
    for (commit, parent) in &commits {
        let paths = match gitrepo::diff_names(&root, parent, commit) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => {
                // an empty commit: both sides run everything
                skipped.push(json!({"commit": commit, "reason": "empty diff"}));
                continue;
            }
            Err(e) => {
                errors.push(json!({"commit": commit, "error": e}));
                continue;
            }
        };
        if let Err(e) = std::fs::write(&paths_file, paths.join("\n") + "\n") {
            errors.push(json!({"commit": commit, "error": e.to_string()}));
            continue;
        }
        let old = match run_old(o, &root, &paths_file, &out_file) {
            Ok(m) => m,
            Err(e) => {
                errors.push(json!({"commit": commit, "error": e}));
                continue;
            }
        };
        let new = classify::classify_paths(&repo, cfg, &paths);
        compared += 1;
        let mut lost = Vec::new();
        let mut gained = false;
        for c in &classes {
            let Some(&o_sel) = old.get(c) else { continue };
            compared_classes.insert(c.clone());
            let n_sel = new.classes.get(c).copied().unwrap_or(true);
            if o_sel && !n_sel {
                lost.push(c.clone());
            } else if n_sel && !o_sel {
                gained = true;
            }
        }
        // An inert path selects no class (inert AND claimed runs everything),
        // so it cannot cause a narrowing: the prefix test sees the other
        // paths, and there must be at least one.
        let live: Vec<&String> = paths
            .iter()
            .filter(|x| !classify::is_inert(cfg, x))
            .collect();
        let (ok_lost, bad_lost): (Vec<String>, Vec<String>) = lost.into_iter().partition(|c| {
            o.accept_narrowing.get(c).is_some_and(|prefixes| {
                prefixes.iter().any(|p| {
                    p.is_empty()
                        || (!live.is_empty() && live.iter().all(|x| x.starts_with(p.as_str())))
                })
            })
        });
        if !ok_lost.is_empty() {
            accepted.push(json!({"commit": commit, "lost": ok_lost, "paths": paths.iter().take(20).collect::<Vec<_>>()}));
        }
        if !bad_lost.is_empty() {
            narrower.push(json!({"commit": commit, "lost": bad_lost, "paths": paths.iter().take(20).collect::<Vec<_>>()}));
        } else if !ok_lost.is_empty() {
            // counted in accepted_narrowings
        } else if gained {
            wider += 1;
        } else {
            identical += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let uncompared: Vec<&String> = classes.difference(&compared_classes).collect();
    let refused = compared == 0 || compared_classes.is_empty();
    if o.format == "json" {
        let out = json!({
            "schema": crate::EQUIVALENCE_SCHEMA,
            "ref": o.refspec,
            "commits": commits.len(),
            "compared": compared,
            "identical": identical,
            "wider": wider,
            "narrower": narrower,
            "accepted_narrowings": accepted,
            // a list of class names, as in v0.2.1; the prefixes are separate
            "accept_narrowing_classes": o.accept_narrowing.keys().collect::<Vec<_>>(),
            "accept_narrowing_prefixes": o.accept_narrowing,
            "skipped": skipped,
            "errors": errors,
            "classes_compared": compared_classes,
            "classes_uncompared": uncompared,
            "refused": refused,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for n in &narrower {
            println!(
                "  NARROWER {} lost {}",
                n["commit"].as_str().unwrap_or(""),
                n["lost"]
            );
        }
        for n in &accepted {
            println!(
                "  ACCEPTED-NARROWER {} lost {} (--accept-narrowing)",
                n["commit"].as_str().unwrap_or(""),
                n["lost"]
            );
        }
        for k in &skipped {
            println!(
                "  SKIPPED {} ({})",
                k["commit"].as_str().unwrap_or(""),
                k["reason"].as_str().unwrap_or("")
            );
        }
        for e in &errors {
            println!(
                "  ERROR {} {}",
                e["commit"].as_str().unwrap_or(""),
                e["error"].as_str().unwrap_or("")
            );
        }
        println!(
            "  {} commit(s), {} compared, {} skipped: {} identical, {} wider, {} narrower, {} accepted narrower ({:?}), {} error(s); classes compared {:?}, uncompared {:?}",
            commits.len(),
            compared,
            skipped.len(),
            identical,
            wider,
            narrower.len(),
            accepted.len(),
            o.accept_narrowing,
            errors.len(),
            compared_classes,
            uncompared
        );
    }
    if refused {
        ExitCode::from(3)
    } else if !narrower.is_empty() || !errors.is_empty() {
        // An error leaves a commit uncompared: not a pass.
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
