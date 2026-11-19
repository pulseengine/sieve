//! `sieve equivalence`, the switch-over gate (REQ-013), on a small fixture
//! history: sieve against itself is identical (exit 0), against an old
//! detector that selects everything it is narrower (exit 1), a failing old
//! detector is an uncompared commit (exit 1), an unknown ref is refused (3).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

fn git(d: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A repo with classes a (a/) and b (b/), and three commits after the root:
/// one touching a/, one touching b/, one touching docs only.
fn history(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("equivalence")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("a")).unwrap();
    fs::create_dir_all(d.join("b")).unwrap();
    fs::write(d.join("a/x"), "1").unwrap();
    fs::write(d.join("b/y"), "1").unwrap();
    fs::write(d.join("README.md"), "# r\n").unwrap();
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "root"]);
    for (p, body) in [("a/x", "2"), ("b/y", "2"), ("README.md", "# r2\n")] {
        fs::write(d.join(p), body).unwrap();
        git(&d, &["commit", "-q", "-am", p]);
    }
    d
}

fn cfg(d: &Path) -> PathBuf {
    let c = d.parent().unwrap().join(format!(
        "{}.sieve.toml",
        d.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &c,
        "[class.a]\nhand = [\"a/\"]\n[class.b]\nhand = [\"b/\"]\n",
    )
    .unwrap();
    c
}

fn script(d: &Path, name: &str, body: &str) -> PathBuf {
    let p = d.parent().unwrap().join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn run(d: &Path, cfg: &Path, old: &Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let o = Command::new(BIN)
        .arg("equivalence")
        .arg("--config")
        .arg(cfg)
        .arg("--root")
        .arg(d)
        // through /bin/sh, not exec'd: another test thread's fork can hold a
        // writable fd on a just-written script, and Linux then refuses to
        // exec it (ETXTBSY, seen in CI on PR #12)
        .args(["--old", "/bin/sh", "--old-arg"])
        .arg(old)
        .args(["--ref", "HEAD", "--commits", "10", "--format", "json"])
        .args(extra)
        .output()
        .unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
    (o.status.code(), v)
}

// rivet: verifies FEAT-005
// rivet: verifies REQ-013
#[test]
fn sieve_against_itself_is_identical() {
    let d = history("self");
    let c = cfg(&d);
    // The old detector is sieve itself, in its github-output mode.
    let old = script(
        &d,
        "self.sh",
        &format!(
            "exec {BIN} classify --config {} --format github-output",
            c.display()
        ),
    );
    let (code, v) = run(&d, &c, &old, &[]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["compared"], 3);
    assert_eq!(v["identical"], 3);
    assert_eq!(v["narrower"].as_array().unwrap().len(), 0);
}

// rivet: verifies REQ-013
#[test]
fn an_old_detector_that_selects_everything_shows_the_narrowings() {
    let d = history("alltrue");
    let c = cfg(&d);
    let old = script(
        &d,
        "alltrue.sh",
        "printf 'a=true\\nb=true\\n' >> \"$GITHUB_OUTPUT\"",
    );
    let (code, v) = run(&d, &c, &old, &[]);
    assert_eq!(code, Some(1), "{v}");
    // a/x selects a only, b/y b only, README.md nothing: all three are narrower.
    assert_eq!(v["narrower"].as_array().unwrap().len(), 3, "{v}");
}

// rivet: verifies REQ-013
#[test]
fn a_mapped_old_output_name_is_compared() {
    let d = history("map");
    let c = cfg(&d);
    let old = script(
        &d,
        "renamed.sh",
        "printf 'alpha=true\\nb=true\\n' >> \"$GITHUB_OUTPUT\"",
    );
    let (code, v) = run(&d, &c, &old, &["--map", "alpha=a"]);
    assert_eq!(code, Some(1), "{v}");
    assert!(
        v["classes_compared"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "a"),
        "{v}"
    );
}

// rivet: verifies REQ-013
#[test]
fn a_failing_old_detector_is_not_a_pass() {
    let d = history("fails");
    let c = cfg(&d);
    let old = script(&d, "fails.sh", "exit 7");
    let (code, v) = run(&d, &c, &old, &[]);
    assert_ne!(code, Some(0), "{v}");
}

// rivet: verifies REQ-013
#[test]
fn an_unknown_ref_is_refused() {
    let d = history("noref");
    let c = cfg(&d);
    let old = script(&d, "x.sh", "true");
    let o = Command::new(BIN)
        .args(["equivalence", "--config"])
        .arg(&c)
        .arg("--root")
        .arg(&d)
        .arg("--old")
        .arg(&old)
        .args(["--ref", "no-such-ref"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3));
}

// rivet: verifies REQ-013
#[test]
fn one_old_output_maps_to_several_classes_and_an_intended_narrowing_can_be_accepted() {
    let d = history("one-to-many");
    let c = cfg(&d);
    // A single-output old detector (like spar's `code`) that is true on every change.
    let old = script(&d, "code.sh", "echo 'code=true' >> \"$GITHUB_OUTPUT\"");
    let (code, v) = run(&d, &c, &old, &["--map", "code=a", "--map", "code=b"]);
    assert_eq!(code, Some(1), "{v}");
    let compared: Vec<&str> = v["classes_compared"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert_eq!(
        compared,
        ["a", "b"],
        "both mapped classes are compared: {v}"
    );
    // a/x drops b, b/y drops a, README drops both: all three are narrower.
    assert_eq!(v["narrower"].as_array().unwrap().len(), 3, "{v}");
    // Declaring both narrowings intended: listed, not failing.
    let (code, v) = run(
        &d,
        &c,
        &old,
        &[
            "--map",
            "code=a",
            "--map",
            "code=b",
            "--accept-narrowing",
            "a",
            "--accept-narrowing",
            "b",
        ],
    );
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["accepted_narrowings"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(v["narrower"].as_array().unwrap().len(), 0, "{v}");
}

// rivet: verifies REQ-013
#[test]
fn an_empty_commit_is_listed_as_skipped_and_an_unknown_class_is_a_usage_error() {
    let d = history("skipped");
    git(&d, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    let c = cfg(&d);
    let old = script(
        &d,
        "self.sh",
        &format!(
            "exec {BIN} classify --config {} --format github-output",
            c.display()
        ),
    );
    let (code, v) = run(&d, &c, &old, &[]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["skipped"][0]["reason"], "empty diff", "{v}");
    let (code, _) = run(&d, &c, &old, &["--map", "x=nope"]);
    assert_eq!(code, Some(2));
}

// rivet: verifies REQ-013
#[test]
fn a_narrowing_can_be_accepted_under_a_path_prefix_only() {
    let d = history("accept-prefix");
    // one more commit touching a/ AND the README: it also drops b
    fs::write(d.join("a/x"), "3").unwrap();
    fs::write(d.join("README.md"), "# r3\n").unwrap();
    git(&d, &["commit", "-q", "-am", "a and readme"]);
    let c = cfg(&d);
    let old = script(&d, "code.sh", "echo 'code=true' >> \"$GITHUB_OUTPUT\"");
    let base = ["--map", "code=a", "--map", "code=b"];
    // b's loss is accepted only where every changed path is under a/
    let mut args = base.to_vec();
    args.extend(["--accept-narrowing", "b:a/"]);
    let (code, v) = run(&d, &c, &old, &args);
    assert_eq!(code, Some(1), "{v}");
    let accepted: Vec<&str> = v["accepted_narrowings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["paths"][0].as_str().unwrap())
        .collect();
    assert_eq!(accepted, ["a/x"], "only the a/-only commit: {v}");
    // narrower: b/y (drops a), README (drops both), a+README (drops b, README outside a/)
    assert_eq!(v["narrower"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(
        v["accept_narrowing_classes"],
        serde_json::json!(["b"]),
        "{v}"
    );
    assert_eq!(
        v["accept_narrowing_prefixes"]["b"],
        serde_json::json!(["a/"]),
        "{v}"
    );
    // an empty prefix is a usage error, not "accept everything"
    let mut args = base.to_vec();
    args.extend(["--accept-narrowing", "b:"]);
    let (code, _) = run(&d, &c, &old, &args);
    assert_eq!(code, Some(2));
}
