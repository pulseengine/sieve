//! sieve.toml: the per-repository classes and rules.
//!
//! Any error here is a usage error (exit 2): a config sieve cannot read must
//! turn the classifier job red, and a red classifier job runs everything.

use std::collections::BTreeMap;
use toml::Value;

/// Built in and not removable: a change to any of these can re-route any job
/// or the CI itself, and a pull request must not classify its own CI or
/// toolchain change away.
pub const DEFAULT_RUN_ALL_PREFIXES: &[&str] = &[".github/"];
pub const DEFAULT_RUN_ALL_FILES: &[&str] = &[
    "varve.toml",
    "varve-realms.toml",
    ".bazelrc",
    ".bazelversion",
    ".bazelignore",
    ".gitmodules",
];
pub const DEFAULT_RUN_ALL_BASENAMES: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "BUILD",
    "BUILD.bazel",
    "MODULE.bazel",
    "MODULE.bazel.lock",
    "WORKSPACE",
    "WORKSPACE.bazel",
    ".bazelversion",
    ".bazelrc",
];
pub const DEFAULT_RUN_ALL_SUFFIXES: &[&str] = &[".bzl"];
/// A directory name that runs everything wherever it sits (`.cargo/config.toml`
/// changes how every crate below it builds).
pub const DEFAULT_RUN_ALL_DIRS: &[&str] = &[".cargo"];

#[derive(Debug, Clone, Default)]
pub struct BazelSnapshot {
    pub file: String,
    pub key: String,
}

#[derive(Debug, Clone, Default)]
pub struct Class {
    pub name: String,
    pub cargo: Vec<String>,
    pub cargo_glob: Vec<String>,
    pub zephyr: Vec<String>,
    pub hand: Vec<String>,
    pub bazel_build: Vec<String>,
    pub bazel_snapshot: Option<BazelSnapshot>,
    pub true_inputs: Vec<String>,
}

/// `[workflows]`: what `sieve check-workflows` needs beyond the classes.
#[derive(Debug, Clone, Default)]
pub struct Workflows {
    /// Repository-relative required-contexts list (CLI `--required` wins).
    pub required_contexts: Option<String>,
    /// HAND-MAINTAINED: required job display name -> the classifier outputs it
    /// may read. check-workflows proves a job reads only its own outputs, not
    /// that the map is right; the false-skip controls test the map's content.
    pub job_outputs: BTreeMap<String, Vec<String>>,
    /// HAND-MAINTAINED: "<workflow file>:<job id>:<step name>" -> reason a
    /// `continue-on-error` step that nothing observes is allowed.
    pub coe_exempt: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Repository-relative path of the config file itself (always run-all).
    pub path: String,
    pub run_all_files: Vec<String>,
    pub run_all_prefixes: Vec<String>,
    pub run_all_suffixes: Vec<String>,
    pub run_all_basenames: Vec<String>,
    pub run_all_dirs: Vec<String>,
    pub inert_files: Vec<String>,
    pub inert_prefixes: Vec<String>,
    pub docs_namer_exclude: Vec<String>,
    pub cmake_root_vars: Vec<String>,
    pub no_job: Vec<String>,
    pub classes: Vec<Class>,
    pub workflows: Workflows,
    /// sha256 of the config bytes.
    pub digest: String,
}

pub type Result<T> = std::result::Result<T, String>;

fn strs(v: &Value, what: &str) -> Result<Vec<String>> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("{what}: expected an array of strings"))?;
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{what}: expected strings"))
        })
        .collect()
}

fn table<'a>(v: &'a Value, what: &str) -> Result<&'a toml::map::Map<String, Value>> {
    v.as_table()
        .ok_or_else(|| format!("{what}: expected a table"))
}

fn check_keys(t: &toml::map::Map<String, Value>, allowed: &[&str], what: &str) -> Result<()> {
    for k in t.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!(
                "{what}: unknown key `{k}` (allowed: {})",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn valid_rel(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.split('/').any(|c| c == ".." || c == ".")
        && !p.contains('\\')
}

pub fn parse(text: &str, rel_path: &str) -> Result<Config> {
    use sha2::{Digest, Sha256};
    let doc: Value = toml::from_str(text).map_err(|e| format!("{rel_path}: {e}"))?;
    let top = table(&doc, rel_path)?;
    check_keys(
        top,
        &[
            "schema",
            "run_all",
            "inert",
            "docs",
            "zephyr",
            "check",
            "workflows",
            "class",
        ],
        rel_path,
    )?;
    if let Some(s) = top.get("schema") {
        if s.as_str() != Some("sieve.config.v1") {
            return Err(format!("{rel_path}: schema must be \"sieve.config.v1\""));
        }
    }
    let mut c = Config {
        path: rel_path.to_string(),
        run_all_files: DEFAULT_RUN_ALL_FILES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        run_all_prefixes: DEFAULT_RUN_ALL_PREFIXES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        run_all_suffixes: DEFAULT_RUN_ALL_SUFFIXES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        run_all_basenames: DEFAULT_RUN_ALL_BASENAMES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        run_all_dirs: DEFAULT_RUN_ALL_DIRS.iter().map(|s| s.to_string()).collect(),
        digest: format!("{:x}", Sha256::digest(text.as_bytes())),
        ..Default::default()
    };
    c.run_all_files.push(rel_path.to_string());
    if let Some(v) = top.get("run_all") {
        let t = table(v, "run_all")?;
        check_keys(
            t,
            &["files", "prefixes", "suffixes", "basenames"],
            "run_all",
        )?;
        for (k, dst) in [
            ("files", &mut c.run_all_files),
            ("prefixes", &mut c.run_all_prefixes),
            ("suffixes", &mut c.run_all_suffixes),
            ("basenames", &mut c.run_all_basenames),
        ] {
            if let Some(v) = t.get(k) {
                dst.extend(strs(v, &format!("run_all.{k}"))?);
            }
        }
    }
    if let Some(v) = top.get("inert") {
        let t = table(v, "inert")?;
        check_keys(t, &["files", "prefixes"], "inert")?;
        if let Some(v) = t.get("files") {
            c.inert_files = strs(v, "inert.files")?;
        }
        if let Some(v) = t.get("prefixes") {
            c.inert_prefixes = strs(v, "inert.prefixes")?;
            if let Some(bad) = c.inert_prefixes.iter().find(|p| !p.ends_with('/')) {
                return Err(format!("inert.prefixes: `{bad}` must end with `/`"));
            }
        }
    }
    if let Some(v) = top.get("docs") {
        let t = table(v, "docs")?;
        check_keys(t, &["namer_exclude"], "docs")?;
        if let Some(v) = t.get("namer_exclude") {
            c.docs_namer_exclude = strs(v, "docs.namer_exclude")?;
        }
    }
    if let Some(v) = top.get("zephyr") {
        let t = table(v, "zephyr")?;
        check_keys(t, &["cmake_root_vars"], "zephyr")?;
        if let Some(v) = t.get("cmake_root_vars") {
            c.cmake_root_vars = strs(v, "zephyr.cmake_root_vars")?;
        }
    }
    if let Some(v) = top.get("check") {
        let t = table(v, "check")?;
        check_keys(t, &["no_job"], "check")?;
        if let Some(v) = t.get("no_job") {
            c.no_job = strs(v, "check.no_job")?;
        }
    }
    let classes = match top.get("class") {
        Some(v) => table(v, "class")?.clone(),
        None => return Err(format!("{rel_path}: no [class.<name>] defined")),
    };
    let ordered: BTreeMap<_, _> = classes.into_iter().collect();
    for (name, v) in ordered {
        if name == "run_all"
            || !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return Err(format!("class `{name}`: not a valid output name"));
        }
        let what = format!("class.{name}");
        let t = table(&v, &what)?;
        check_keys(
            t,
            &[
                "cargo",
                "cargo_glob",
                "zephyr",
                "hand",
                "bazel_build",
                "bazel_snapshot",
                "true_inputs",
            ],
            &what,
        )?;
        let mut cl = Class {
            name: name.clone(),
            ..Default::default()
        };
        for (k, dst) in [
            ("cargo", &mut cl.cargo),
            ("cargo_glob", &mut cl.cargo_glob),
            ("zephyr", &mut cl.zephyr),
            ("hand", &mut cl.hand),
            ("bazel_build", &mut cl.bazel_build),
            ("true_inputs", &mut cl.true_inputs),
        ] {
            if let Some(v) = t.get(k) {
                *dst = strs(v, &format!("{what}.{k}"))?;
                if let Some(bad) = dst.iter().find(|p| !valid_rel(p.trim_end_matches('/'))) {
                    return Err(format!(
                        "{what}.{k}: `{bad}` is not a repository-relative path"
                    ));
                }
            }
        }
        if let Some(v) = t.get("bazel_snapshot") {
            let s = table(v, &format!("{what}.bazel_snapshot"))?;
            check_keys(s, &["file", "key"], &format!("{what}.bazel_snapshot"))?;
            let get = |k: &str| {
                s.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("{what}.bazel_snapshot.{k}: expected a string"))
            };
            cl.bazel_snapshot = Some(BazelSnapshot {
                file: get("file")?,
                key: get("key")?,
            });
        }
        if cl.cargo.is_empty()
            && cl.cargo_glob.is_empty()
            && cl.zephyr.is_empty()
            && cl.hand.is_empty()
            && cl.bazel_build.is_empty()
            && cl.bazel_snapshot.is_none()
        {
            return Err(format!("{what}: declares no inputs"));
        }
        c.classes.push(cl);
    }
    if let Some(v) = top.get("workflows") {
        c.workflows = parse_workflows(table(v, "workflows")?, &c.classes)?;
    }
    Ok(c)
}

fn parse_workflows(t: &toml::map::Map<String, Value>, classes: &[Class]) -> Result<Workflows> {
    check_keys(
        t,
        &["required_contexts", "job_outputs", "coe_exempt"],
        "workflows",
    )?;
    let mut w = Workflows::default();
    if let Some(v) = t.get("required_contexts") {
        let p = v
            .as_str()
            .ok_or("workflows.required_contexts: expected a string")?;
        if !valid_rel(p) {
            return Err(format!(
                "workflows.required_contexts: `{p}` is not a repository-relative path"
            ));
        }
        w.required_contexts = Some(p.to_string());
    }
    if let Some(v) = t.get("job_outputs") {
        for (job, outs) in table(v, "workflows.job_outputs")? {
            let what = format!("workflows.job_outputs.\"{job}\"");
            let outs = strs(outs, &what)?;
            if outs.is_empty() {
                return Err(format!("{what}: names no classifier output"));
            }
            if let Some(bad) = outs.iter().find(|o| !classes.iter().any(|c| &c.name == *o)) {
                return Err(format!("{what}: `{bad}` is not a [class.<name>]"));
            }
            w.job_outputs.insert(job.clone(), outs);
        }
    }
    if let Some(v) = t.get("coe_exempt") {
        for (key, why) in table(v, "workflows.coe_exempt")? {
            let what = format!("workflows.coe_exempt.\"{key}\"");
            if key.splitn(3, ':').count() != 3 {
                return Err(format!(
                    "{what}: key must be <workflow file>:<job id>:<step name>"
                ));
            }
            match why.as_str() {
                Some(r) if !r.trim().is_empty() => {
                    w.coe_exempt.insert(key.clone(), r.to_string());
                }
                _ => return Err(format!("{what}: a non-empty reason string is required")),
            }
        }
    }
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_key_is_an_error() {
        let e = parse("[class.a]\nhand = [\"x/\"]\nsrc = []\n", "sieve.toml").unwrap_err();
        assert!(e.contains("unknown key `src`"), "{e}");
    }

    #[test]
    fn defaults_cannot_be_dropped() {
        let c = parse(
            "[run_all]\nprefixes = [\"tools/\"]\n[class.a]\nhand = [\"x/\"]\n",
            "sieve.toml",
        )
        .unwrap();
        assert!(c.run_all_prefixes.contains(&".github/".to_string()));
        assert!(c.run_all_files.contains(&"sieve.toml".to_string()));
    }

    #[test]
    fn workflows_table_is_validated() {
        let ok = "[workflows]\nrequired_contexts = \".github/required-contexts.txt\"\n[workflows.job_outputs]\n\"Build\" = [\"a\"]\n[class.a]\nhand = [\"x/\"]\n";
        let c = parse(ok, "sieve.toml").unwrap();
        assert_eq!(c.workflows.job_outputs["Build"], vec!["a".to_string()]);
        let e = parse(&ok.replace("[\"a\"]", "[\"nope\"]"), "sieve.toml").unwrap_err();
        assert!(e.contains("not a [class.<name>]"), "{e}");
        let e = parse(
            &ok.replace("[workflows]", "[workflows]\nbogus = 1"),
            "sieve.toml",
        )
        .unwrap_err();
        assert!(e.contains("unknown key `bogus`"), "{e}");
    }

    #[test]
    fn no_class_is_an_error() {
        assert!(parse("", "sieve.toml").is_err());
    }
}
