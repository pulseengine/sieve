//! CLI contract tests beyond the corpus: the crash path, exit codes, --version.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

fn tree(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("cli")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("a/src")).unwrap();
    fs::write(
        d.join("a/Cargo.toml"),
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(d.join("a/src/lib.rs"), "pub fn a() {}\n").unwrap();
    fs::write(d.join("README.md"), "# r\n").unwrap();
    fs::write(
        d.join("sieve.toml"),
        "[class.a]\ncargo = [\"a/Cargo.toml\"]\n[class.b]\nhand = [\"b/\"]\n",
    )
    .unwrap();
    d
}

fn classify(
    dir: &PathBuf,
    paths: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, serde_json::Value, String) {
    let mut c = Command::new(BIN);
    c.args(["classify", "--config", "sieve.toml", "--paths"])
        .args(paths)
        .current_dir(dir);
    c.env_remove("GATE_PROBE_FILES").env_remove("SIEVE_FAULT");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
    (
        o.status.code(),
        v,
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

// rivet: verifies REQ-001
// rivet: verifies REQ-008
#[test]
fn a_classified_change_selects_only_its_class() {
    let d = tree("plain");
    let (code, v, _) = classify(&d, &["a/src/lib.rs"], &[]);
    assert_eq!(code, Some(0));
    assert_eq!(v["run_all"], false);
    assert_eq!(v["classes"]["a"], true);
    assert_eq!(v["classes"]["b"], false);
    assert!(v["inputs_hash"].as_str().is_some_and(|h| h.len() == 64));
}

// rivet: verifies REQ-003
#[test]
fn a_panic_selects_everything_with_an_internal_reason_and_a_warning() {
    let d = tree("panic");
    let (code, v, stderr) = classify(&d, &["a/src/lib.rs"], &[("SIEVE_FAULT", "panic")]);
    assert_eq!(code, Some(0));
    assert_eq!(v["run_all"], true);
    assert_eq!(v["classes"]["a"], true);
    assert_eq!(v["classes"]["b"], true);
    assert!(
        v["reason"].as_str().unwrap().starts_with("internal:"),
        "{}",
        v["reason"]
    );
    assert!(stderr.contains("::warning"), "{stderr}");
}

// rivet: verifies REQ-005
#[test]
fn a_github_path_is_a_path_reason_not_internal() {
    let d = tree("github");
    let (_, v, stderr) = classify(&d, &[".github/workflows/ci.yml"], &[]);
    assert_eq!(v["run_all"], true);
    let r = v["reason"].as_str().unwrap();
    assert!(
        r.starts_with("run-all path:") && !r.starts_with("internal:"),
        "{r}"
    );
    assert!(!stderr.contains("::warning"));
}

// rivet: verifies REQ-005
#[test]
fn the_config_file_itself_runs_everything() {
    let d = tree("selfconfig");
    let (_, v, _) = classify(&d, &["sieve.toml"], &[]);
    assert_eq!(v["run_all"], true);
}

// rivet: verifies REQ-005
#[test]
fn nested_cargo_dir_runs_everything() {
    let d = tree("dotcargo");
    let (_, v, _) = classify(&d, &["a/.cargo/config.toml"], &[]);
    assert_eq!(v["run_all"], true);
}

// rivet: verifies REQ-003
#[test]
fn an_invalid_config_exits_2() {
    let d = tree("badcfg");
    fs::write(d.join("sieve.toml"), "[class.a]\nsrcs = []\n").unwrap();
    let (code, v, _) = classify(&d, &["a/src/lib.rs"], &[]);
    assert_eq!(code, Some(2));
    assert!(v.is_null(), "no verdict may be printed on a usage error");
}

// rivet: verifies REQ-011
#[test]
fn version_names_the_schemas() {
    let o = Command::new(BIN).arg("--version").output().unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    for schema in [
        "sieve.classify.v1",
        "sieve.graph.v1",
        "sieve.check.v1",
        "sieve.config.v1",
    ] {
        assert!(s.contains(schema), "{s}");
    }
}

// rivet: verifies REQ-009
#[test]
fn graph_reports_sources() {
    let d = tree("graph");
    let o = Command::new(BIN)
        .args(["graph", "--config", "sieve.toml"])
        .current_dir(&d)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["schema"], "sieve.graph.v1");
    assert_eq!(v["classes"]["a"]["sources"]["a/"], "cargo");
    assert_eq!(v["classes"]["b"]["sources"]["b/"], "hand");
}

// rivet: verifies REQ-004
#[test]
fn a_path_with_a_control_character_runs_everything_and_cannot_forge_a_command() {
    let d = tree("ctrl");
    let p = "a/x\r::error::forged.rs";
    let (_, v, stderr) = classify(&d, &[p], &[]);
    assert_eq!(v["run_all"], true);
    assert!(!stderr.contains("\r"), "raw CR reached the log: {stderr:?}");
    assert!(
        !stderr.lines().any(|l| l.starts_with("::error")),
        "{stderr:?}"
    );
}

// rivet: verifies REQ-023
#[test]
fn toolchain_pins_dep_info_cannot_see_run_everything() {
    let d = tree("pins");
    // b/ is claimed by class b, so without the rule each pin would select b only.
    for p in [
        "b/flake.nix",
        "b/flake.lock",
        "b/lean-toolchain",
        "b/rust-toolchain.toml",
        "b/Cargo.lock",
    ] {
        let (_, v, _) = classify(&d, &[p], &[]);
        assert_eq!(v["run_all"], true, "{p} must select everything");
    }
}

// rivet: verifies FEAT-004
// rivet: verifies REQ-021
#[test]
fn an_unresolved_input_is_in_the_json_with_a_code() {
    let d = tree("unresolved-json");
    fs::write(
        d.join("a/src/lib.rs"),
        "include!(concat!(env!(\"SOMEWHERE\"), \"/x.rs\"));\n",
    )
    .unwrap();
    let (code, v, _) = classify(&d, &["README.md"], &[]);
    assert_eq!(code, Some(0));
    assert_eq!(v["classes"]["a"], true, "an unresolved class always runs");
    let u = &v["unresolved"]["a"][0];
    assert_eq!(u["code"], "include-unresolved", "{v}");
    assert!(u["input"].as_str().unwrap().contains("a/src/lib.rs"), "{u}");
    assert!(v.get("explain").is_none(), "explain only with --explain");
}

// rivet: verifies REQ-020
#[test]
fn explain_names_the_path_the_claim_and_the_edge() {
    let d = tree("explain");
    let o = Command::new(BIN)
        .args([
            "classify",
            "--config",
            "sieve.toml",
            "--explain",
            "--paths",
            "a/src/lib.rs",
        ])
        .current_dir(&d)
        .env_remove("GATE_PROBE_FILES")
        .env_remove("SIEVE_FAULT")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let e = &v["explain"]["a"][0];
    assert_eq!(e["path"], "a/src/lib.rs", "{v}");
    assert_eq!(e["claim"], "a/");
    assert_eq!(e["via"], "cargo");
    assert!(
        v["explain"].get("b").is_none(),
        "class b was not selected: {v}"
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("explain a: a/src/lib.rs via cargo (claim a/)"),
        "{stderr}"
    );
}

// rivet: verifies REQ-009
#[test]
fn an_unreadable_index_refuses_check_graph_instead_of_checking_nothing() {
    let d = tree("bad-index");
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "x"]);
    fs::write(d.join(".git/index"), "garbage").unwrap();
    let o = Command::new(BIN)
        .args([
            "check-graph",
            "--config",
            "sieve.toml",
            "--control",
            "graph-inputs",
            "--format",
            "json",
        ])
        .current_dir(&d)
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["refused"][0]["control"], "graph-inputs", "{v}");
}

// rivet: verifies REQ-010
// rivet: verifies REQ-018
#[test]
fn paths_mode_reads_the_index_when_there_is_one_and_walks_only_without_git() {
    let d = tree("index-first");
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "x"]);
    // Untracked residue that names the doc (an agent log, bytecode, ...).
    fs::write(d.join("untracked-notes.txt"), "see README.md").unwrap();
    let (_, v, _) = classify(&d, &["README.md"], &[]);
    assert_eq!(
        v["run_all"], false,
        "with a repository, untracked files are not read: {v}"
    );
    fs::remove_dir_all(d.join(".git")).unwrap();
    let (_, v, _) = classify(&d, &["README.md"], &[]);
    assert_eq!(
        v["run_all"], true,
        "without .git the walk reads it and the doc is not docs-only: {v}"
    );
}
