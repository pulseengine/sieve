//! Cross-class builds (REQ-026), the case spar's switch-over found: a gated
//! job that builds another class's Cargo package must have a class claiming
//! that package's manifest, or a change to that package skips the job.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

/// spar/ is a Rust package (class rust). The Lean job's class claims lean/
/// only, unless `lean_cargo` also gives it spar's manifest.
fn tree(name: &str, lean_cargo: bool, lean_run: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("cross-class")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    for (p, body) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"spar\"]\n"),
        (
            "spar/Cargo.toml",
            "[package]\nname = \"spar\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("spar/src/lib.rs", "pub fn gen() {}\n"),
        ("lean/Check.lean", "theorem t : True := trivial\n"),
        (".github/required-contexts.txt", "Lean\n"),
    ] {
        let f = d.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, body).unwrap();
    }
    let extra = if lean_cargo {
        "cargo = [\"spar/Cargo.toml\"]\n"
    } else {
        ""
    };
    fs::write(
        d.join("sieve.toml"),
        format!(
            "[class.rust]\ncargo = [\"spar/Cargo.toml\"]\n[class.lean]\nhand = [\"lean/\"]\n{extra}\
             [workflows]\nrequired_contexts = \".github/required-contexts.txt\"\n\
             [workflows.job_outputs]\n\"Lean\" = [\"lean\"]\n"
        ),
    )
    .unwrap();
    let wf = format!(
        "on:\n  pull_request:\n  merge_group:\njobs:\n  classify:\n    runs-on: x\n    outputs:\n      lean: ${{{{ steps.c.outputs.lean }}}}\n      run_all: ${{{{ steps.c.outputs.run_all }}}}\n    steps:\n      - id: c\n        run: sieve classify --config sieve.toml --format github-output\n  lean:\n    name: Lean\n    needs: classify\n    if: ${{{{ !cancelled() && (needs.classify.result != 'success' || needs.classify.outputs.lean != 'false') }}}}\n    runs-on: x\n    steps:\n      - run: {lean_run}\n"
    );
    fs::create_dir_all(d.join(".github/workflows")).unwrap();
    fs::write(d.join(".github/workflows/lean.yml"), wf).unwrap();
    d
}

fn codes(d: &Path) -> Vec<String> {
    let o = Command::new(BIN)
        .args([
            "check-workflows",
            "--config",
            "sieve.toml",
            "--format",
            "json",
        ])
        .current_dir(d)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
    v["failures"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|f| f["gate"] == "d")
                .filter_map(|f| f["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// rivet: verifies REQ-026
#[test]
fn a_job_that_builds_another_class_package_must_claim_it() {
    // spar's pre-fix draft: the Lean job builds spar but claims only lean/
    for (name, run) in [
        ("p", "cargo build -p spar && lake build"),
        ("package-eq", "cargo run --release --package=spar -- gen"),
        ("manifest", "cargo build --manifest-path spar/Cargo.toml"),
        ("versioned", "cargo test -p spar@0.1.0"),
    ] {
        let c = codes(&tree(&format!("draft-{name}"), false, run));
        assert_eq!(c, vec!["cross-class-build".to_string()], "{name}: {c:?}");
    }
    // after the fix (the Lean class claims spar's Cargo closure) it passes
    for (name, run) in [
        ("p", "cargo build -p spar && lake build"),
        ("manifest", "cargo build --manifest-path spar/Cargo.toml"),
    ] {
        let c = codes(&tree(&format!("fixed-{name}"), true, run));
        assert!(c.is_empty(), "{name}: {c:?}");
    }
    // a job that builds nothing by name is not this check's business
    assert!(codes(&tree("no-cargo", false, "lake build")).is_empty());
}

// rivet: verifies REQ-026
#[test]
fn what_cannot_be_resolved_fails_closed() {
    let c = codes(&tree("unknown", true, "cargo build -p ghost"));
    assert_eq!(c, vec!["cross-class-unresolved".to_string()], "{c:?}");
    let c = codes(&tree("expr", true, "cargo build -p ${{ matrix.crate }}"));
    assert_eq!(c, vec!["cross-class-not-literal".to_string()], "{c:?}");
    let c = codes(&tree(
        "escape",
        true,
        "cargo build --manifest-path ../x/Cargo.toml",
    ));
    assert_eq!(c, vec!["cross-class-unresolved".to_string()], "{c:?}");
}

/// spar with a test and a path dev-dependency; the Lean class is given by
/// `lean_class` (a sieve.toml fragment for [class.lean]).
fn closure_tree(name: &str, lean_class: &str, lean_run: &str) -> PathBuf {
    let d = tree(name, false, lean_run);
    for (p, body) in [
        (
            "spar/Cargo.toml",
            "[package]\nname = \"spar\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dev-dependencies]\nfixtures = { path = \"../fixtures\" }\n",
        ),
        ("spar/tests/gen.rs", "#[test] fn t() {}\n"),
        ("fixtures/Cargo.toml", "[package]\nname = \"fixtures\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("fixtures/src/lib.rs", "\n"),
        (
            "tool/Cargo.toml",
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nspar = { path = \"../spar\" }\n",
        ),
        ("tool/src/main.rs", "fn main() {}\n"),
        ("Cargo.toml", "[workspace]\nmembers = [\"spar\", \"fixtures\", \"tool\"]\n"),
    ] {
        let f = d.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, body).unwrap();
    }
    fs::write(
        d.join("sieve.toml"),
        format!(
            "[class.rust]\ncargo = [\"spar/Cargo.toml\"]\n[class.lean]\n{lean_class}\
             [workflows]\nrequired_contexts = \".github/required-contexts.txt\"\n\
             [workflows.job_outputs]\n\"Lean\" = [\"lean\"]\n"
        ),
    )
    .unwrap();
    d
}

// rivet: verifies REQ-026
#[test]
fn claiming_the_manifest_alone_does_not_cover_what_the_job_builds() {
    // the manifest is claimed, spar's sources are not
    let c = codes(&closure_tree(
        "manifest-only",
        "hand = [\"lean/\", \"spar/Cargo.toml\"]\n",
        "cargo build -p spar",
    ));
    assert_eq!(c, vec!["cross-class-build".to_string()], "{c:?}");
    // spar reached only as a normal dependency of tool: its tests and its
    // dev-dependency are not claimed, but `cargo test -p spar` builds them
    let c = codes(&closure_tree(
        "via-dependency",
        "hand = [\"lean/\"]\ncargo = [\"tool/Cargo.toml\"]\n",
        "cargo test -p spar",
    ));
    assert_eq!(c, vec!["cross-class-build".to_string()], "{c:?}");
    // a class with spar as a root claims all of it, dev-dependency included
    let c = codes(&closure_tree(
        "root",
        "hand = [\"lean/\"]\ncargo = [\"spar/Cargo.toml\"]\n",
        "cargo test -p spar",
    ));
    assert!(c.is_empty(), "{c:?}");
}
