//! `sieve check-workflows`: one passing baseline and a planted defect per
//! rule. Every plant must make the gate exit 1 with the expected gate and
//! code, and nothing else; the baseline and the positives must exit 0.
//!
//! The fixtures are written into CARGO_TARGET_TMPDIR (a throwaway git repo
//! each, because gate (c) runs sieve's own classification on the tree).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

const RUN: &str = "sieve classify --config sieve.toml --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output";
const GOOD_IF: &str =
    "${{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false') }}";
const STEP_ENV: &str = "        env:\n          EVENT_NAME: ${{ github.event_name }}\n          BASE_SHA: ${{ github.event.pull_request.base.sha }}\n";

fn toml(extra_gate_output: bool) -> String {
    let gate = if extra_gate_output {
        "[\"zephyr\", \"proofs\"]"
    } else {
        "[\"zephyr\"]"
    };
    format!(
        "[class.zephyr]\nhand = [\"z/\"]\n[class.proofs]\nhand = [\"p/\"]\n\
[workflows]\nrequired_contexts = \".github/required-contexts.txt\"\n\
# HAND-MAINTAINED: required job name -> the classifier outputs it may read.\n\
[workflows.job_outputs]\n\"The Gate\" = {gate}\n\"Other Gate\" = [\"zephyr\"]\n\
[workflows.coe_exempt]\n\"w.yml:g:trend\" = \"test exemption\"\n"
    )
}

/// A workflow with a classifier job, a gate job, and a second gate job.
#[derive(Clone)]
struct W {
    trigger: String,
    wf_extra: String,
    cjob_extra: String,
    outputs: Vec<&'static str>,
    out_step: &'static str,
    step_extra: String,
    step_env: String,
    run: String,
    cjob_steps: String,
    other_if: Option<String>,
    gate_name: String,
    needs: Option<String>,
    gate_if: Option<String>,
    gate_extra: String,
    gate_steps: String,
}

impl W {
    fn base() -> W {
        W {
            trigger: "on:\n  pull_request:\n    branches: [main]\n  merge_group:\n".into(),
            wf_extra: String::new(),
            cjob_extra: String::new(),
            outputs: vec!["zephyr"],
            out_step: "classify",
            step_extra: String::new(),
            step_env: STEP_ENV.into(),
            run: RUN.into(),
            cjob_steps: String::new(),
            other_if: None,
            gate_name: "The Gate".into(),
            needs: Some("changes".into()),
            gate_if: Some(GOOD_IF.into()),
            gate_extra: String::new(),
            gate_steps: "    steps: [{run: 'true'}]\n".into(),
        }
    }
    fn render(&self) -> String {
        let mut y = format!("name: w\n{}{}jobs:\n", self.trigger, self.wf_extra);
        y += &format!(
            "  changes:\n    runs-on: ubuntu-24.04\n{}    outputs:\n",
            self.cjob_extra
        );
        for o in &self.outputs {
            y += &format!(
                "      {o}: ${{{{ steps.{}.outputs.{o} }}}}\n",
                self.out_step
            );
        }
        y += &format!(
            "    steps:\n      - id: classify\n{}{}        run: {}\n",
            self.step_extra, self.step_env, self.run
        );
        y += &self.cjob_steps;
        y += "  other:\n    name: \"Other Gate\"\n    runs-on: ubuntu-24.04\n";
        if let Some(i) = &self.other_if {
            y += &format!("    needs: changes\n    if: {i}\n");
        }
        y += "    outputs:\n      x: ${{ steps.s.outputs.x }}\n    steps:\n      - id: s\n        run: echo x=false >> $GITHUB_OUTPUT\n";
        y += &format!("  g:\n    name: \"{}\"\n", self.gate_name);
        if let Some(n) = &self.needs {
            y += &format!("    needs: {n}\n");
        }
        if let Some(i) = &self.gate_if {
            y += &format!("    if: {i}\n");
        }
        y += &format!(
            "    runs-on: ubuntu-24.04\n{}{}",
            self.gate_extra, self.gate_steps
        );
        y
    }
}

struct Fixture {
    root: PathBuf,
}

fn fixture(name: &str, workflow: &str, required: &str, cfg: &str, with_git: bool) -> Fixture {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("wfgates")
        .join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".github/workflows")).unwrap();
    fs::write(root.join(".github/workflows/w.yml"), workflow).unwrap();
    fs::write(root.join(".github/required-contexts.txt"), required).unwrap();
    fs::write(root.join("sieve.toml"), cfg).unwrap();
    if with_git {
        for args in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        ] {
            let o = Command::new("git")
                .args(&args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {o:?}");
        }
    }
    Fixture { root }
}

struct Out {
    code: Option<i32>,
    json: serde_json::Value,
    stdout: String,
}

fn check_in(root: &Path, extra: &[&str]) -> Out {
    let o = Command::new(BIN)
        .args([
            "check-workflows",
            "--config",
            "sieve.toml",
            "--format",
            "json",
        ])
        .args(extra)
        .current_dir(root)
        .env_remove("GATE_PROBE_FILES")
        .env_remove("SIEVE_FAULT")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    Out {
        code: o.status.code(),
        json: serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null),
        stdout,
    }
}

fn run(name: &str, w: &W, required: &str) -> Out {
    let f = fixture(name, &w.render(), required, &toml(false), true);
    check_in(&f.root, &[])
}

fn found(o: &Out) -> BTreeSet<(String, String)> {
    o.json["failures"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            (
                f["gate"].as_str().unwrap_or("?").to_string(),
                f["code"].as_str().unwrap_or("?").to_string(),
            )
        })
        .collect()
}

/// Every plant exits 1 with exactly its expected (gate, code); returns the
/// names of the plants that did not.
fn missed(plants: Vec<(&str, W, &str, &str, &str)>) -> Vec<String> {
    let mut bad = vec![];
    for (name, w, required, gate, code) in plants {
        let o = run(&format!("plant-{name}"), &w, required);
        let want: BTreeSet<(String, String)> = [(gate.to_string(), code.to_string())].into();
        if o.code != Some(1) || found(&o) != want {
            bad.push(format!(
                "{name}: exit {:?}, expected exit 1 with {want:?}, got {:?}\n{}",
                o.code,
                found(&o),
                o.stdout
            ));
        }
    }
    bad
}

const ONE: &str = "The Gate\n";
const BOTH: &str = "The Gate\nOther Gate\n";

fn trig(pr: &str, rest: &str) -> String {
    format!("on:\n  pull_request:\n{pr}{rest}")
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn the_baseline_passes_and_reports_its_checks() {
    let o = run("baseline", &W::base(), ONE);
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert_eq!(o.json["schema"], "sieve.workflows.v1");
    assert_eq!(o.json["pass"], true);
    assert!(o.json["checks"].as_u64().unwrap() >= 7, "{}", o.stdout);
    assert_eq!(o.json["failures"].as_array().unwrap().len(), 0);
    assert_eq!(o.json["refused"].as_array().unwrap().len(), 0);
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn gate_a_a_workflow_that_may_not_report_is_caught() {
    let mg = "  merge_group:\n";
    let w = |trigger: String| W {
        trigger,
        ..W::base()
    };
    let bad = missed(vec![
        (
            "dangling-paths",
            w(trig("    branches: [main]\n    paths:\n", mg)),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "real-paths",
            w(trig("    branches: [main]\n    paths: ['src/**']\n", mg)),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "paths-ignore",
            w(trig("    paths-ignore: ['docs/**']\n", mg)),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "push-paths",
            w(format!("on:\n  pull_request:\n    branches: [main]\n  push:\n    branches: [main]\n    paths: ['src/**']\n{mg}")),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "push-paths-ignore",
            w(format!("on:\n  pull_request:\n  push:\n    paths-ignore: ['docs/**']\n{mg}")),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "no-pr",
            w(format!("on:\n  push:\n    branches: [main]\n{mg}")),
            ONE,
            "a",
            "no-pr",
        ),
        (
            "types-drop-synchronize",
            w(trig("    branches: [main]\n    types: [opened]\n", mg)),
            ONE,
            "a",
            "types-restricted",
        ),
        (
            "branch-excludes-main",
            w(trig("    branches: [develop]\n", mg)),
            ONE,
            "a",
            "branch-mismatch",
        ),
        (
            "branches-ignore-main",
            w(trig("    branches-ignore: [main]\n", mg)),
            ONE,
            "a",
            "branch-mismatch",
        ),
        (
            "no-merge-group",
            w(trig("    branches: [main]\n", "")),
            ONE,
            "a",
            "no-merge-group",
        ),
        (
            "merge-group-drops-checks-requested",
            w(trig(
                "    branches: [main]\n",
                "  merge_group:\n    types: [destroyed]\n",
            )),
            ONE,
            "a",
            "no-merge-group",
        ),
        (
            // a bare `pull_request:` fires on everything, but still not in the queue
            "bare-pull-request-no-queue",
            w("on:\n  pull_request:\n".into()),
            ONE,
            "a",
            "no-merge-group",
        ),
        (
            "string-on-no-queue",
            w("on: pull_request\n".into()),
            ONE,
            "a",
            "no-merge-group",
        ),
        (
            "list-on-no-queue",
            w("on: [pull_request, push]\n".into()),
            ONE,
            "a",
            "no-merge-group",
        ),
        (
            // YAML 1.1 reads the key `on` as boolean true; the gate must see through it
            "bool-true-key-paths",
            w("true:\n  pull_request:\n    paths: ['src/**']\n  merge_group:\n".into()),
            ONE,
            "a",
            "paths-filter",
        ),
        (
            "context-no-job-produces",
            W::base(),
            "The Gate\nA Context Nothing Produces\n",
            "a",
            "unmapped",
        ),
        (
            "required-job-renamed",
            W {
                gate_name: "The Gate (renamed)".into(),
                ..W::base()
            },
            ONE,
            "a",
            "unmapped",
        ),
    ]);
    assert!(bad.is_empty(), "plants not caught:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn gate_b_the_if_must_be_the_classifier_form() {
    let gif = |s: &str| W {
        gate_if: Some(s.into()),
        ..W::base()
    };
    let bad = missed(vec![
        ("if-false", gif("false"), ONE, "b", "if-form"),
        (
            "if-event-term",
            gif("${{ !cancelled() && github.event_name != 'merge_group' && (needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false') }}"),
            ONE, "b", "if-form",
        ),
        (
            "old-form-without-result-term",
            gif("${{ !cancelled() && needs.changes.outputs.zephyr != 'false' }}"),
            ONE, "b", "if-form",
        ),
        (
            "parenthesised-without-result-term",
            gif("${{ !cancelled() && (needs.changes.outputs.zephyr != 'false') }}"),
            ONE, "b", "if-form",
        ),
        (
            "eq-true",
            gif("${{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.zephyr == 'true') }}"),
            ONE, "b", "if-form",
        ),
        (
            "no-cancelled",
            gif("${{ needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false' }}"),
            ONE, "b", "if-form",
        ),
        (
            "always-instead-of-not-cancelled",
            gif("${{ always() && (needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false') }}"),
            ONE, "b", "if-form",
        ),
        (
            "disjunction-not-parenthesised",
            gif("${{ !cancelled() && needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false' }}"),
            ONE, "b", "if-form",
        ),
        (
            "if-mixes-two-classifier-jobs",
            W {
                needs: Some("[changes, other]".into()),
                ..gif("${{ !cancelled() && (needs.changes.result != 'success' || needs.other.outputs.x != 'false') }}")
            },
            ONE, "b", "if-form",
        ),
        (
            "if-reads-no-output",
            gif("${{ !cancelled() && (needs.changes.result != 'success') }}"),
            ONE, "b", "if-form",
        ),
        (
            "extra-skip-term-inside-the-disjunction",
            gif("${{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false' || github.event_name != 'merge_group') }}"),
            ONE, "b", "if-form",
        ),
        (
            "if-is-a-boolean",
            gif("true"),
            ONE, "b", "if-form",
        ),
        (
            "if-reads-a-job-that-is-not-the-classifier",
            W {
                needs: Some("[changes, other]".into()),
                ..gif("${{ !cancelled() && (needs.other.result != 'success' || needs.other.outputs.x != 'false') }}")
            },
            ONE, "b", "classifier-not-run",
        ),
        (
            "if-reads-a-job-it-does-not-need",
            W { needs: None, ..W::base() },
            ONE, "b", "classifier-not-needed",
        ),
        (
            "output-wired-to-another-step",
            W { out_step: "other", ..W::base() },
            ONE, "b", "output-unwired",
        ),
        (
            "reads-another-gates-output",
            W {
                outputs: vec!["zephyr", "proofs"],
                ..gif("${{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.proofs != 'false') }}")
            },
            ONE, "b", "output-not-allowed",
        ),
        (
            "gated-job-not-on-the-map",
            W { gate_name: "Third Gate".into(), ..W::base() },
            "Third Gate\n", "b", "job-not-mapped",
        ),
    ]);
    assert!(bad.is_empty(), "plants not caught:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn gate_b_a_skip_cannot_come_in_through_needs_steps_or_matrix() {
    let steps = |s: &str| W {
        gate_if: None,
        needs: None,
        gate_steps: s.into(),
        ..W::base()
    };
    let bad = missed(vec![
        (
            "need-not-required",
            W { gate_if: None, needs: Some("other".into()), ..W::base() },
            ONE, "b", "needs-not-required",
        ),
        (
            "needs-classifier-without-if",
            W { gate_if: None, ..W::base() },
            ONE, "b", "needs-classifier-without-if",
        ),
        (
            "need-can-be-skipped-and-no-form",
            W {
                gate_if: None,
                needs: Some("other".into()),
                other_if: Some(GOOD_IF.into()),
                ..W::base()
            },
            BOTH, "b", "needs-gated-job",
        ),
        (
            "step-if-reads-needs-output",
            steps("    steps:\n      - if: needs.changes.outputs.zephyr == 'true'\n        run: 'true'\n"),
            ONE, "b", "step-if-output",
        ),
        (
            "step-if-reads-step-output",
            steps("    steps:\n      - id: q\n        run: echo run=false >> $GITHUB_OUTPUT\n      - if: steps.q.outputs.run == 'true'\n        run: 'true'\n"),
            ONE, "b", "step-if-output",
        ),
        (
            "step-if-reads-needs-by-index-and-case",
            steps("    steps:\n      - if: ${{ NEEDS['changes'].outputs.zephyr == 'true' }}\n        run: 'true'\n"),
            ONE, "b", "step-if-output",
        ),
        (
            "step-if-reads-step-output-by-index",
            steps("    steps:\n      - id: q\n        run: echo run=false >> $GITHUB_OUTPUT\n      - if: steps['q'].Outputs.run == 'true'\n        run: 'true'\n"),
            ONE, "b", "step-if-output",
        ),
        (
            "continue-on-error-on-the-job",
            W {
                gate_if: None,
                needs: None,
                gate_extra: "    continue-on-error: true\n".into(),
                ..W::base()
            },
            ONE, "b", "continue-on-error-job",
        ),
        (
            "continue-on-error-on-an-unobserved-step",
            steps("    steps:\n      - id: v\n        continue-on-error: true\n        run: 'false'\n"),
            ONE, "b", "continue-on-error-step",
        ),
        (
            "matrix-exclude",
            W {
                gate_extra: "    strategy:\n      matrix:\n        a: [1]\n        exclude:\n          - a: 1\n".into(),
                ..W::base()
            },
            ONE, "b", "matrix-unprovable",
        ),
        (
            "matrix-empty-axis",
            W {
                gate_extra: "    strategy:\n      matrix:\n        a: []\n".into(),
                ..W::base()
            },
            ONE, "b", "matrix-unprovable",
        ),
        (
            "matrix-is-an-expression",
            W {
                gate_extra: "    strategy:\n      matrix: ${{ fromJSON(needs.changes.outputs.m) }}\n".into(),
                ..W::base()
            },
            ONE, "b", "matrix-unprovable",
        ),
    ]);
    assert!(bad.is_empty(), "plants not caught:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn gate_b_the_classifier_step_must_run_exactly_sieve_classify() {
    let step_env_plus = |extra: &str| format!("{STEP_ENV}{extra}");
    let run = |r: &str| W {
        run: r.into(),
        ..W::base()
    };
    let bad = missed(vec![
        (
            "run-appended-echo",
            run(&format!("{RUN} && echo zephyr=false >> $GITHUB_OUTPUT")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-pipes-the-classifier",
            run(&format!("{RUN} | tee out.txt")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-reads-a-different-config",
            run("sieve classify --config other.toml --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output"),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-appends-a-path-list",
            run(&format!("{RUN} --paths docs/x.md")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-single-quotes-the-event-so-the-shell-never-expands-it",
            run("sieve classify --config sieve.toml --base \"$BASE_SHA\" --event '$EVENT_NAME' --format github-output"),
            ONE, "b", "classifier-wiring",
        ),
        (
            "two-steps-run-the-classifier",
            W {
                cjob_steps: "      - run: sieve classify --config sieve.toml --paths x --format github-output\n".into(),
                ..W::base()
            },
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-classifies-a-fixed-path-list",
            run("sieve classify --config sieve.toml --paths docs/x.md --event \"$EVENT_NAME\" --format github-output"),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-hard-codes-the-event",
            run("sieve classify --config sieve.toml --base \"$BASE_SHA\" --event pull_request --format github-output"),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-writes-json-not-github-output",
            run("sieve classify --config sieve.toml --base \"$BASE_SHA\" --event \"$EVENT_NAME\""),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-into-github-output",
            run(&format!("{RUN} --record \"$GITHUB_OUTPUT\"")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-into-github-env",
            run(&format!("{RUN} --record $GITHUB_ENV")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-absolute-path",
            run(&format!("{RUN} --record /tmp/v.json")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-dotdot",
            run(&format!("{RUN} --record \"$RUNNER_TEMP/../v.json\"")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-twice",
            run(&format!("{RUN} --record a.json --record b.json")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "record-without-a-value",
            run(&format!("{RUN} --record")),
            ONE, "b", "classifier-wiring",
        ),
        (
            "run-without-a-base",
            run("sieve classify --config sieve.toml --event \"$EVENT_NAME\" --format github-output"),
            ONE, "b", "classifier-wiring",
        ),
        (
            "env-hard-codes-event-name",
            W {
                step_env: "        env:\n          EVENT_NAME: pull_request\n          BASE_SHA: ${{ github.event.pull_request.base.sha }}\n".into(),
                ..W::base()
            },
            ONE, "b", "classifier-wiring",
        ),
        (
            "env-sets-gate-probe-files",
            W { step_env: step_env_plus("          GATE_PROBE_FILES: docs.txt\n"), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
        (
            "job-env-sets-gate-probe-files",
            W { cjob_extra: "    env:\n      GATE_PROBE_FILES: docs.txt\n".into(), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
        (
            "workflow-env-sets-event-name",
            W { wf_extra: "env:\n  EVENT_NAME: pull_request\n".into(), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
        (
            "classify-step-has-if",
            W { step_extra: "        if: github.event_name == 'pull_request'\n".into(), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
        (
            "classify-step-continue-on-error",
            W { step_extra: "        continue-on-error: true\n".into(), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
        (
            "classify-job-continue-on-error",
            W { cjob_extra: "    continue-on-error: true\n".into(), ..W::base() },
            ONE, "b", "classifier-wiring",
        ),
    ]);
    assert!(bad.is_empty(), "plants not caught:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn the_accepted_forms_pass() {
    let mut w2 = W::base();
    w2.outputs = vec!["zephyr", "proofs"];
    w2.gate_if = Some("${{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.zephyr != 'false' || needs.changes.outputs.proofs != 'false') }}".into());
    let steps = |s: &str| W {
        gate_if: None,
        needs: None,
        gate_steps: s.into(),
        ..W::base()
    };
    let cases: Vec<(&str, W, bool)> = vec![
        ("two-outputs-both-on-the-map", w2, true),
        ("no-if-no-needs", steps("    steps: [{run: 'true'}]\n"), false),
        (
            "observed-continue-on-error",
            steps("    steps:\n      - id: v\n        continue-on-error: true\n        run: 'true'\n      - run: test \"${{ steps.v.outcome }}\" = success\n"),
            false,
        ),
        (
            "exempt-continue-on-error",
            steps("    steps:\n      - name: trend\n        continue-on-error: true\n        run: 'true'\n"),
            false,
        ),
        (
            "a-non-empty-matrix",
            W { gate_extra: "    strategy:\n      matrix:\n        a: [1, 2]\n        include:\n          - a: 3\n".into(), ..W::base() },
            false,
        ),
        (
            "flags-in-another-order-and-braced-vars",
            W {
                run: "./target/release/sieve classify --format github-output --event \"${EVENT_NAME}\" --base \"${BASE_SHA}\" --config ./sieve.toml".into(),
                ..W::base()
            },
            false,
        ),
        (
            "record-in-runner-temp",
            W { run: format!("{RUN} --record \"$RUNNER_TEMP/sieve-verdict.json\""), ..W::base() },
            false,
        ),
        (
            "record-braced-runner-temp-first",
            W { run: "sieve classify --record ${RUNNER_TEMP}/v.json --config sieve.toml --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output".into(), ..W::base() },
            false,
        ),
        (
            "record-relative-path",
            W { run: format!("{RUN} --record out/sieve-verdict.json"), ..W::base() },
            false,
        ),
        (
            "push-without-paths",
            W { trigger: "on:\n  pull_request:\n  push:\n    branches: [main]\n  merge_group:\n".into(), ..W::base() },
            false,
        ),
        (
            "bool-true-key-is-on",
            W { trigger: "true:\n  pull_request:\n    branches: [main]\n  merge_group:\n".into(), ..W::base() },
            false,
        ),
        (
            "list-trigger-with-the-queue",
            W { trigger: "on: [pull_request, merge_group]\n".into(), ..W::base() },
            false,
        ),
        (
            "bare-pull-request-with-the-queue",
            W { trigger: "on:\n  pull_request:\n  merge_group:\n".into(), ..W::base() },
            false,
        ),
        (
            "matrix-suffixed-context-name",
            W {
                gate_extra: "    strategy:\n      matrix:\n        a: [1, 2]\n".into(),
                ..W::base()
            },
            false,
        ),
    ];
    let mut bad = vec![];
    for (name, w, two) in cases {
        let f = fixture(
            &format!("pos-{name}"),
            &w.render(),
            if name == "matrix-suffixed-context-name" {
                "The Gate (1)\n"
            } else {
                ONE
            },
            &toml(two),
            true,
        );
        let o = check_in(&f.root, &[]);
        if o.code != Some(0) {
            bad.push(format!("{name}: exit {:?}\n{}", o.code, o.stdout));
        }
    }
    assert!(bad.is_empty(), "false positives:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn gate_c_runs_sieve_on_the_tree_and_refuses_without_git() {
    // The classifier is sieve itself, so a plant cannot make it fail closed;
    // what this proves is that (c) really runs on the tree: without .git the
    // spot checks cannot run, and the gate refuses rather than pass.
    let f = fixture("no-git", &W::base().render(), ONE, &toml(false), false);
    let o = check_in(&f.root, &[]);
    assert_eq!(o.code, Some(3), "{}", o.stdout);
    assert_eq!(o.json["pass"], false);
    assert_eq!(o.json["refused"][0]["input"], "c");
    // A tree with nothing gated by the classifier has nothing to run (c) on.
    let w = W {
        gate_if: None,
        needs: None,
        ..W::base()
    };
    let f = fixture(
        "no-git-no-classifier",
        &w.render(),
        ONE,
        &toml(false),
        false,
    );
    assert_eq!(check_in(&f.root, &[]).code, Some(0));
}

// rivet: verifies REQ-014
// rivet: verifies FEAT-002
#[test]
fn missing_inputs_are_refused_and_bad_usage_is_exit_2() {
    let f = fixture("inputs", &W::base().render(), ONE, &toml(false), true);
    let o = check_in(&f.root, &["--required", "nope.txt"]);
    assert_eq!(o.code, Some(3), "{}", o.stdout);
    assert_eq!(
        o.json["refused"][0]["input"]
            .as_str()
            .map(|s| s.ends_with("nope.txt")),
        Some(true)
    );
    let o = check_in(&f.root, &["--workflows", "no/such/dir"]);
    assert_eq!(o.code, Some(3), "{}", o.stdout);
    // --required and --workflows are honoured, and relative to --root.
    fs::create_dir_all(f.root.join("alt")).unwrap();
    fs::copy(
        f.root.join(".github/workflows/w.yml"),
        f.root.join("alt/w.yml"),
    )
    .unwrap();
    fs::write(f.root.join("alt/req.txt"), ONE).unwrap();
    let o = check_in(
        &f.root,
        &["--workflows", "alt", "--required", "alt/req.txt"],
    );
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    for bad in [
        vec!["--format", "yaml"],
        vec!["--bogus"],
        vec!["--workflows"],
    ] {
        let o = check_in(&f.root, &bad);
        assert_eq!(o.code, Some(2), "{bad:?}: {}", o.stdout);
    }
    // sieve.toml: an unknown key under [workflows] stays an error, and a
    // job_outputs entry must name a real class.
    for cfg in [
        format!("{}\n[workflows.nope]\n", toml(false)),
        toml(false).replace("[\"zephyr\"]", "[\"nothing\"]"),
    ] {
        let g = fixture("bad-config", &W::base().render(), ONE, &cfg, true);
        assert_eq!(check_in(&g.root, &[]).code, Some(2));
    }
    // An unparseable workflow is a failure, not something to skip silently.
    fs::write(
        f.root.join(".github/workflows/broken.yml"),
        "on: [unclosed\n  - x: : :\n",
    )
    .unwrap();
    let o = check_in(&f.root, &[]);
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    assert!(found(&o).contains(&("a".to_string(), "unparseable".to_string())));
}

// rivet: verifies REQ-014
#[test]
fn matrix_names_and_unconditional_ifs_are_accepted() {
    let include = "    strategy:\n      fail-fast: false\n      matrix:\n        include:\n          - { label: linux, runner: ubuntu-24.04 }\n          - { label: macos, runner: macos-latest }\n          - { label: windows, runner: windows-latest }\n";
    let lists =
        "    strategy:\n      matrix:\n        os: [linux, macos]\n        rust: [stable, beta]\n";
    let cases: Vec<(&str, W, &str)> = vec![
        (
            // sieve#30: relay's Test (${{ matrix.label }}) over include
            "matrix-include-label",
            W {
                gate_name: "The Gate (${{ matrix.label }})".into(),
                gate_extra: include.into(),
                ..W::base()
            },
            "The Gate (linux)\nThe Gate (macos)\nThe Gate (windows)\n",
        ),
        (
            "matrix-lists",
            W {
                gate_name: "The Gate (${{ matrix.os }}, ${{matrix.rust}})".into(),
                gate_extra: lists.into(),
                ..W::base()
            },
            "The Gate (linux, stable)\nThe Gate (linux, beta)\nThe Gate (macos, stable)\nThe Gate (macos, beta)\n",
        ),
        (
            "if-always",
            W {
                gate_if: Some("always()".into()),
                ..W::base()
            },
            ONE,
        ),
        (
            "if-not-cancelled-wrapped",
            W {
                gate_if: Some("${{ !cancelled() }}".into()),
                ..W::base()
            },
            ONE,
        ),
    ];
    let mut bad = vec![];
    for (name, w, required) in cases {
        let f = fixture(
            &format!("pos30-{name}"),
            &w.render(),
            required,
            &toml(false),
            true,
        );
        let o = check_in(&f.root, &[]);
        if o.code != Some(0) {
            bad.push(format!("{name}: exit {:?}\n{}", o.code, o.stdout));
        }
    }
    assert!(bad.is_empty(), "false positives:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-014
#[test]
fn matrix_names_do_not_invent_contexts() {
    let lists = "    strategy:\n      matrix:\n        os: [linux, macos]\n        rust: [stable, beta]\n        exclude:\n          - { os: macos, rust: beta }\n";
    let bad = missed(vec![
        (
            // the excluded row is not produced
            "matrix-excluded-row",
            W {
                gate_name: "The Gate (${{ matrix.os }}, ${{ matrix.rust }})".into(),
                gate_extra: lists.into(),
                ..W::base()
            },
            "The Gate (macos, beta)\n",
            "a",
            "unmapped",
        ),
        (
            // a name with a non-matrix expression stays unexpanded
            "name-with-other-expression",
            W {
                gate_name: "The Gate (${{ github.ref }})".into(),
                ..W::base()
            },
            "The Gate (refs/heads/main)\n",
            "a",
            "unmapped",
        ),
    ]);
    assert!(bad.is_empty(), "plants not caught:\n{}", bad.join("\n"));
}
