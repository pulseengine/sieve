//! Never-skip guard jobs (REQ-028): a job a North Star guard requires to run
//! (hardware run, evidence verifier negative control, proof check,
//! equivalence job) must not be skippable. Each plant must turn the gate red.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

const BASE: &str = "on:\n  pull_request:\n  merge_group:\njobs:\n";

fn repo(name: &str, workflow: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("never-skip")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join(".github/workflows")).unwrap();
    fs::write(d.join(".github/workflows/hw.yml"), workflow).unwrap();
    fs::write(d.join(".github/required-contexts.txt"), "Lint\n").unwrap();
    fs::write(
        d.join("sieve.toml"),
        "[workflows]\nnever_skip_jobs = [\"Hardware run\"]\n[class.a]\nhand = [\"a/\"]\n",
    )
    .unwrap();
    d
}

fn codes(d: &PathBuf) -> (Option<i32>, Vec<String>) {
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
    let codes = v["failures"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (o.status.code(), codes)
}

const LINT: &str = "  lint:\n    name: Lint\n    runs-on: x\n    steps:\n      - run: true\n";

// rivet: verifies REQ-028
// rivet: verifies FEAT-007
#[test]
fn an_unconditional_guard_job_passes() {
    let wf = format!("{BASE}{LINT}  hw:\n    name: Hardware run\n    runs-on: x\n    steps:\n      - run: true\n");
    let (code, c) = codes(&repo("ok", &wf));
    assert_eq!(code, Some(0), "{c:?}");
    let wf = format!("{BASE}{LINT}  hw:\n    name: Hardware run\n    if: ${{{{ !cancelled() }}}}\n    runs-on: x\n    steps:\n      - run: true\n");
    let (code, c) = codes(&repo("ok-not-cancelled", &wf));
    assert_eq!(code, Some(0), "!cancelled() keeps the job running: {c:?}");
}

// rivet: verifies REQ-028
#[test]
fn every_way_to_make_a_guard_job_skippable_is_caught() {
    let cases: [(&str, String, &str); 4] = [
        (
            "event-if",
            format!("{BASE}{LINT}  hw:\n    name: Hardware run\n    if: github.event_name == 'push'\n    runs-on: x\n    steps:\n      - run: true\n"),
            "never-skip-if",
        ),
        (
            "skippable-need",
            format!(
                "{BASE}{LINT}  gate:\n    if: github.event_name == 'push'\n    runs-on: x\n    steps:\n      - run: true\n  hw:\n    name: Hardware run\n    needs: [gate]\n    runs-on: x\n    steps:\n      - run: true\n"
            ),
            "never-skip-needs",
        ),
        ("missing", format!("{BASE}{LINT}"), "never-skip-missing"),
        (
            "no-merge-group",
            format!("on:\n  pull_request:\njobs:\n{LINT}  hw:\n    name: Hardware run\n    runs-on: x\n    steps:\n      - run: true\n"),
            "never-skip-",
        ),
    ];
    for (name, wf, want) in cases {
        let (code, c) = codes(&repo(name, &wf));
        assert_eq!(code, Some(1), "{name}: {c:?}");
        assert!(
            c.iter().any(|x| x.starts_with(want)),
            "{name}: want {want}, got {c:?}"
        );
    }
}
