//! `sieve observe`, the observed-miss counter (REQ-019).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("observe")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("sieve.toml"),
        "[class.a]\nhand = [\"a/\"]\n[class.b]\nhand = [\"b/\"]\n\
         [workflows.job_outputs]\n\"Heavy A\" = [\"a\"]\n\"Heavy B\" = [\"b\"]\n",
    )
    .unwrap();
    d
}

/// Verdict: a skipped, b selected (unless run_all).
fn verdict(d: &Path, run_all: bool) -> PathBuf {
    let p = d.join("verdict.json");
    let v = serde_json::json!({
        "schema": "sieve.classify.v1", "run_all": run_all, "reason": null,
        "classes": {"a": run_all, "b": true}, "unknown_paths": [], "inputs_hash": null, "unresolved": {}
    });
    fs::write(&p, v.to_string()).unwrap();
    p
}

fn observe(d: &Path, verdict: &Path, jobs: serde_json::Value) -> (Option<i32>, serde_json::Value) {
    let r = d.join("jobs.json");
    fs::write(&r, jobs.to_string()).unwrap();
    let o = Command::new(BIN)
        .args([
            "observe",
            "--config",
            "sieve.toml",
            "--format",
            "json",
            "--verdict",
        ])
        .arg(verdict)
        .arg("--results")
        .arg(&r)
        .current_dir(d)
        .output()
        .unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
    (o.status.code(), v)
}

// rivet: verifies FEAT-006
// rivet: verifies REQ-019
#[test]
fn a_failing_job_whose_classes_were_all_skipped_is_a_miss() {
    let d = dir("miss");
    let v = verdict(&d, false);
    let (code, out) = observe(
        &d,
        &v,
        serde_json::json!([{"name": "Heavy A", "conclusion": "failure"}]),
    );
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(out["misses"][0]["job"], "Heavy A");
    // The API shape and a matrix suffix are understood too.
    let (code, out) = observe(
        &d,
        &v,
        serde_json::json!({"jobs": [{"name": "Heavy A (ubuntu-24.04)", "conclusion": "timed_out"}]}),
    );
    assert_eq!(code, Some(1), "{out}");
}

// rivet: verifies REQ-019
#[test]
fn passing_selected_ungated_or_run_all_jobs_are_not_misses() {
    let d = dir("nomiss");
    let v = verdict(&d, false);
    let jobs = serde_json::json!([
        {"name": "Heavy A", "conclusion": "success"},
        {"name": "Heavy B", "conclusion": "failure"},
        {"name": "Lint", "conclusion": "failure"}
    ]);
    let (code, out) = observe(&d, &v, jobs.clone());
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(out["gated_failed_jobs"], 1, "{out}");
    let v_all = verdict(&d, true);
    let (code, out) = observe(
        &d,
        &v_all,
        serde_json::json!([{"name": "Heavy A", "conclusion": "failure"}]),
    );
    assert_eq!(code, Some(0), "run_all selects everything: {out}");
}

// rivet: verifies REQ-019
#[test]
fn nothing_to_judge_is_refused() {
    let d = dir("refused");
    let v = verdict(&d, false);
    let (code, _) = observe(
        &d,
        &v,
        serde_json::json!([{"name": "Lint", "conclusion": "failure"}]),
    );
    assert_eq!(code, Some(3), "no gated job");
    fs::write(d.join("bad.json"), "{\"schema\": \"other\"}").unwrap();
    let (code, _) = observe(&d, &d.join("bad.json"), serde_json::json!([]));
    assert_eq!(code, Some(3), "not a classify verdict");
}
