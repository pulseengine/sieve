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

// rivet: verifies REQ-028
#[test]
fn a_guard_job_cannot_be_skipped_or_emptied_indirectly() {
    let hw = "  hw:\n    name: Hardware run\n";
    let steps = "    runs-on: x\n    steps:\n      - run: true\n";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "transitive-need",
            format!("{BASE}{LINT}  a:\n    if: github.event_name == 'push'\n{steps}  b:\n    needs: [a]\n{steps}{hw}    needs: [b]\n{steps}"),
            "never-skip-needs",
        ),
        ("missing-need", format!("{BASE}{LINT}{hw}    needs: [ghost]\n{steps}"), "never-skip-needs"),
        (
            "step-if",
            format!("{BASE}{LINT}{hw}    runs-on: x\n    steps:\n      - if: needs.c.outputs.hw == 'true'\n        run: ./flash.sh\n"),
            "never-skip-step-if",
        ),
        ("continue-on-error", format!("{BASE}{LINT}{hw}    continue-on-error: true\n{steps}"), "never-skip-continue-on-error"),
        (
            "matrix-expression",
            format!("{BASE}{LINT}{hw}    strategy:\n      matrix: ${{{{ fromJSON(needs.c.outputs.boards) }}}}\n{steps}"),
            "never-skip-matrix",
        ),
        (
            "matrix-empty-axis",
            format!("{BASE}{LINT}{hw}    strategy:\n      matrix:\n        board: []\n{steps}"),
            "never-skip-matrix",
        ),
        ("reusable", format!("{BASE}{LINT}{hw}    uses: ./.github/workflows/hw-impl.yml\n"), "never-skip-reusable"),
        (
            "wrapped-not-exact",
            format!("{BASE}{LINT}{hw}    if: ${{{{ always() && false }}}}\n{steps}"),
            "never-skip-if",
        ),
    ];
    for (name, wf, want) in cases {
        let (code, c) = codes(&repo(name, &wf));
        assert_eq!(code, Some(1), "{name}: {c:?}");
        assert!(
            c.iter().any(|x| x == want),
            "{name}: want {want}, got {c:?}"
        );
    }
    // ${{ always() }} wrapped once is accepted, as is an unconditional need chain.
    let ok = format!(
        "{BASE}{LINT}  a:\n{steps}{hw}    needs: [a]\n    if: ${{{{ always() }}}}\n{steps}"
    );
    let (code, c) = codes(&repo("ok-chain", &ok));
    assert_eq!(code, Some(0), "{c:?}");
}

// rivet: verifies REQ-028
#[test]
fn a_failed_need_or_an_emptied_matrix_cannot_skip_a_guard_job() {
    let hw = "  hw:\n    name: Hardware run\n";
    let steps = "    runs-on: x\n    steps:\n      - run: true\n";
    let cases: Vec<(&str, String, &str)> = vec![
        // a needs b with no if of its own: a FAILED b skips hw (implicit success())
        ("need-without-always", format!("{BASE}{LINT}  a:\n{steps}{hw}    needs: [a]\n{steps}"), "never-skip-needs"),
        (
            "matrix-exclude",
            format!("{BASE}{LINT}{hw}    strategy:\n      matrix:\n        board: [f4]\n        exclude:\n          - board: f4\n{steps}"),
            "never-skip-matrix",
        ),
        ("strategy-expression", format!("{BASE}{LINT}{hw}    strategy: ${{{{ fromJSON(needs.c.outputs.s) }}}}\n{steps}"), "never-skip-matrix"),
        (
            "include-not-list",
            format!("{BASE}{LINT}{hw}    strategy:\n      matrix:\n        include: ${{{{ fromJSON(needs.c.outputs.i) }}}}\n{steps}"),
            "never-skip-matrix",
        ),
    ];
    for (name, wf, want) in cases {
        let (code, c) = codes(&repo(name, &wf));
        assert_eq!(code, Some(1), "{name}: {c:?}");
        assert!(
            c.iter().any(|x| x == want),
            "{name}: want {want}, got {c:?}"
        );
    }
    let ok = format!("{BASE}{LINT}{hw}    strategy:\n      matrix:\n        include:\n          - board: f4\n{steps}");
    let (code, c) = codes(&repo("ok-include", &ok));
    assert_eq!(code, Some(0), "{c:?}");
}

// rivet: verifies REQ-025
#[test]
fn workflows_parse_with_rivet_yaml_including_whitespace_only_lines() {
    // rivet#1289 (fixed in rivet v0.43.0): a whitespace-only line between
    // block mapping entries is valid YAML; kiln's workflows have them.
    let wf = format!(
        "{BASE}{LINT}  hw:\n    name: Hardware run\n    \n    runs-on: x\n      \n    steps:\n      - run: true\n"
    );
    let (code, c) = codes(&repo("whitespace-line", &wf));
    assert_eq!(code, Some(0), "{c:?}");
}
