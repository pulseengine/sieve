//! REQ-030: a modification of one leaf workflow that leaves its triggers, its
//! classifier job and every job's gating unchanged selects only the classes of
//! the jobs it touches. Each negative control is an edit that must still
//! select everything. Every case is a real two-commit git history classified
//! in git mode (the base version of the workflow is read through gix).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

const CLASSIFIER: &str = "  changes:
    runs-on: ubuntu-latest
    outputs:
      a: ${{ steps.c.outputs.a }}
      b: ${{ steps.c.outputs.b }}
    steps:
      - uses: actions/checkout@v4
      - id: c
        run: sieve classify --config sieve.toml --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output
        env:
          EVENT_NAME: ${{ github.event_name }}
          BASE_SHA: ${{ github.event.pull_request.base.sha }}
";

fn gate(class: &str) -> String {
    format!(
        "${{{{ !cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.{class} != 'false') }}}}"
    )
}

/// ci.yml: classifier `changes`; `ja` gated on a, `jb` gated on b, `lint`
/// ungated, `report` ungated and needing jb.
fn ci() -> String {
    format!(
        "name: ci
on:
  pull_request:
  merge_group:
jobs:
{CLASSIFIER}  ja:
    needs: changes
    if: {ga}
    runs-on: ubuntu-latest
    steps:
      - run: make a
  jb:
    needs: changes
    if: {gb}
    runs-on: ubuntu-latest
    steps:
      - run: make b
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: make lint
  report:
    needs: [jb]
    runs-on: ubuntu-latest
    steps:
      - run: make report
",
        ga = gate("a"),
        gb = gate("b"),
    )
}

const CFG_ON: &str =
    "[class.a]\nhand = [\"a/\"]\n[class.b]\nhand = [\"b/\"]\n[workflows]\nscope_leaf_edits = true\n";
const CFG_OFF: &str = "[class.a]\nhand = [\"a/\"]\n[class.b]\nhand = [\"b/\"]\n";

fn git(d: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A repo whose root commit holds `before` (path -> text), then one commit
/// applying `after` (path -> Some(text) writes, None deletes). Returns the
/// repo and the root commit's sha.
fn history(
    name: &str,
    cfg: &str,
    before: &[(&str, String)],
    after: &[(&str, Option<String>)],
) -> (PathBuf, String) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("leaf_scope")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    let write = |p: &str, t: &str| {
        let f = d.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, t).unwrap();
    };
    write("sieve.toml", cfg);
    write("a/x", "1");
    write("b/y", "1");
    for (p, t) in before {
        write(p, t);
    }
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "root"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    for (p, t) in after {
        match t {
            Some(t) => write(p, t),
            None => fs::remove_file(d.join(p)).unwrap(),
        }
    }
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "--allow-empty", "-m", "edit"]);
    (d, base)
}

fn classify_git(d: &Path, base: &str) -> serde_json::Value {
    let o = Command::new(BIN)
        .args([
            "classify",
            "--config",
            "sieve.toml",
            "--base",
            base,
            "--event",
            "pull_request",
            "--format",
            "json",
            "--explain",
        ])
        .env_remove("GATE_PROBE_FILES")
        .env_remove("SIEVE_FAULT")
        .current_dir(d)
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}

fn selected(v: &serde_json::Value) -> Vec<String> {
    v["classes"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, s)| s.as_bool() == Some(true))
        .map(|(c, _)| c.clone())
        .collect()
}

/// The CI workflow edited from `ci()` by replacing `from` with `to`.
fn edit(name: &str, cfg: &str, from: &str, to: &str) -> serde_json::Value {
    let old = ci();
    assert!(old.contains(from), "fixture lacks {from:?}");
    let new = old.replacen(from, to, 1);
    let (d, base) = history(
        name,
        cfg,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    classify_git(&d, &base)
}

fn assert_scoped(v: &serde_json::Value, want: &[&str]) {
    assert_eq!(v["run_all"], false, "{v}");
    assert_eq!(selected(v), want, "{v}");
}

fn assert_run_all(v: &serde_json::Value, why: &str) {
    assert_eq!(v["run_all"], true, "{v}");
    let reason = v["reason"].as_str().unwrap_or("");
    assert!(reason.contains(why), "reason {reason:?} lacks {why:?}");
}

// rivet: verifies REQ-030
#[test]
fn a_step_edit_in_a_gated_job_selects_only_its_class() {
    let v = edit("step-a", CFG_ON, "make a", "make a V=1");
    assert_scoped(&v, &["a"]);
    assert_eq!(v["explain"]["a"][0]["claim"], "leaf workflow", "{v}");
}

// rivet: verifies REQ-030
#[test]
fn a_step_edit_in_an_ungated_job_selects_no_class() {
    // lint runs on every pull request anyway; no gated job reads it.
    let v = edit("step-lint", CFG_ON, "make lint", "make lint V=1");
    assert_scoped(&v, &[]);
}

// rivet: verifies REQ-030
#[test]
fn a_changed_job_brings_its_dependents_and_their_needs() {
    // jb changes; report needs jb; both are covered by class b.
    let v = edit("step-b", CFG_ON, "make b", "make b V=1");
    assert_scoped(&v, &["b"]);
    // report changes: it is ungated, but it needs jb, which a skip of b
    // would skip, so b is selected.
    let v = edit("step-report", CFG_ON, "make report", "make report V=1");
    assert_scoped(&v, &["b"]);
}

// rivet: verifies REQ-030
#[test]
fn an_added_job_selects_its_own_gate_and_its_needs() {
    let add = |name: &str, job: &str| edit(name, CFG_ON, "  lint:\n", &format!("{job}  lint:\n"));
    let v = add(
        "add-ungated",
        "  extra:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    );
    assert_scoped(&v, &[]);
    let v = add(
        "add-gated",
        &format!(
            "  extra:\n    needs: changes\n    if: {}\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
            gate("b")
        ),
    );
    assert_scoped(&v, &["b"]);
    // Ungated, but it needs ja: a skip of a would skip it.
    let v = add(
        "add-needs",
        "  extra:\n    needs: [ja]\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    );
    assert_scoped(&v, &["a"]);
}

// rivet: verifies REQ-030
#[test]
fn a_scoped_edit_combines_with_other_changed_paths() {
    let old = ci();
    let new = old.replacen("make a", "make a V=1", 1);
    let (d, base) = history(
        "combined",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[
            (".github/workflows/ci.yml", Some(new)),
            ("b/y", Some("2".into())),
        ],
    );
    assert_scoped(&classify_git(&d, &base), &["a", "b"]);
}

// rivet: verifies REQ-030
#[test]
fn gating_edits_still_select_everything() {
    let cases: &[(&str, &str, &str, &str)] = &[
        ("if", &gate("a"), &gate("b"), "changed its `if:`"),
        (
            "coe",
            "  ja:\n    needs: changes\n",
            "  ja:\n    continue-on-error: true\n    needs: changes\n",
            "changed its `continue-on-error:`",
        ),
        (
            "needs",
            "needs: [jb]",
            "needs: [jb, ja]",
            "changed its `needs:`",
        ),
        (
            "matrix",
            "      - run: make a\n",
            "      - run: make a\n    strategy:\n      matrix:\n        x: ${{ fromJSON('[]') }}\n",
            "changed its `strategy:`",
        ),
        (
            "step-if",
            "      - run: make a\n",
            "      - run: make a\n        if: github.event_name == 'push'\n",
            "a step's `if:`",
        ),
        (
            "trigger",
            "  pull_request:\n",
            "  pull_request:\n    paths: [a/**]\n",
            "changed outside `jobs:`",
        ),
        (
            "env",
            "name: ci\n",
            "name: ci\nenv:\n  EVENT_NAME: push\n",
            "changed outside `jobs:`",
        ),
        (
            "classifier",
            "      - uses: actions/checkout@v4\n",
            "      - uses: actions/checkout@v5\n",
            "runs `sieve classify` and changed",
        ),
        (
            "add-classifier",
            "  lint:\n",
            "  c2:\n    runs-on: ubuntu-latest\n    steps:\n      - run: sieve classify --paths x\n  lint:\n",
            "changed which jobs run `sieve classify`",
        ),
        (
            "add-odd-if",
            "  lint:\n",
            "  extra:\n    needs: changes\n    if: needs.changes.outputs.a == 'true'\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n  lint:\n",
            "job extra",
        ),
        (
            "comment",
            "      - run: make a\n",
            "      # a comment\n      - run: make a\n",
            "no job changed",
        ),
        (
            "merge-key",
            "  lint:\n    runs-on: ubuntu-latest\n",
            "  lint:\n    <<: {runs-on: ubuntu-latest}\n",
            "merge key",
        ),
    ];
    for (name, from, to, why) in cases {
        let v = edit(&format!("neg-{name}"), CFG_ON, from, to);
        assert_run_all(&v, why);
    }
}

// rivet: verifies REQ-030
#[test]
fn removing_a_job_or_the_workflow_selects_everything() {
    let old = ci();
    let start = old.find("  lint:\n").unwrap();
    let end = old.find("  report:\n").unwrap();
    let new = format!("{}{}", &old[..start], &old[end..]);
    let (d, base) = history(
        "neg-remove-job",
        CFG_ON,
        &[(".github/workflows/ci.yml", old.clone())],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    assert_run_all(&classify_git(&d, &base), "removes jobs");

    let (d, base) = history(
        "neg-delete",
        CFG_ON,
        &[(".github/workflows/ci.yml", old.clone())],
        &[(".github/workflows/ci.yml", None)],
    );
    assert_run_all(&classify_git(&d, &base), "is deleted");

    let (d, base) = history(
        "neg-add",
        CFG_ON,
        &[],
        &[(".github/workflows/ci.yml", Some(old))],
    );
    assert_run_all(&classify_git(&d, &base), "is added");
}

// rivet: verifies REQ-030
#[test]
fn a_gated_job_without_the_classifier_form_selects_everything() {
    // gale's current form: an extra merge_group term the classifier does not own.
    let odd = "${{ !cancelled() && github.event_name != 'merge_group' && needs.changes.outputs.a != 'false' }}";
    let old = ci().replacen(&gate("a"), odd, 1);
    let new = old.replacen("make a", "make a V=1", 1);
    let (d, base) = history(
        "neg-odd-if",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    assert_run_all(&classify_git(&d, &base), "job ja");
}

// rivet: verifies REQ-030
#[test]
fn a_step_gated_on_outputs_in_a_changed_job_selects_everything() {
    // The step's if: is unchanged, but it reads class b: running ja for a
    // could still skip the edited step.
    let gated_step = "      - run: make a\n        if: needs.changes.outputs.b != 'false'\n";
    let old = ci().replacen("      - run: make a\n", gated_step, 1);
    let new = old.replacen("make a", "make a V=1", 1);
    let (d, base) = history(
        "neg-step-output",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    assert_run_all(
        &classify_git(&d, &base),
        "step gated on a context other than github or runner",
    );
}

// rivet: verifies REQ-030
#[test]
fn reusable_workflows_and_actions_select_everything() {
    let old = ci().replacen("  merge_group:\n", "  merge_group:\n  workflow_call:\n", 1);
    let new = old.replacen("make a", "make a V=1", 1);
    let (d, base) = history(
        "neg-reusable",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    assert_run_all(&classify_git(&d, &base), "reusable workflow");

    let action = "runs:\n  using: composite\n  steps:\n    - run: make\n      shell: bash\n";
    let (d, base) = history(
        "neg-action",
        CFG_ON,
        &[(".github/actions/x/action.yml", action.into())],
        &[(
            ".github/actions/x/action.yml",
            Some(action.replace("make", "make V=1")),
        )],
    );
    assert_run_all(&classify_git(&d, &base), "is CI, toolchain");
}

// rivet: verifies REQ-030
#[test]
fn scoping_is_opt_in_and_needs_git_mode() {
    let v = edit("off", CFG_OFF, "make a", "make a V=1");
    assert_run_all(&v, "is CI, toolchain");

    // An adopter's own run_all entry for the file wins.
    let cfg = format!("{CFG_ON}[run_all]\nfiles = [\".github/workflows/ci.yml\"]\n");
    let cfg = cfg.replacen("[workflows]\nscope_leaf_edits = true\n", "", 1)
        + "[workflows]\nscope_leaf_edits = true\n";
    let v = edit("own-run-all", &cfg, "make a", "make a V=1");
    assert_run_all(&v, "is CI, toolchain");
    // So does an adopter-written `.github/` prefix, though it is also built in.
    let cfg = format!("{CFG_ON}[run_all]\nprefixes = [\".github/\"]\n");
    let cfg = cfg.replacen("[workflows]\nscope_leaf_edits = true\n", "", 1)
        + "[workflows]\nscope_leaf_edits = true\n";
    let v = edit("own-github-prefix", &cfg, "make a", "make a V=1");
    assert_run_all(&v, "is CI, toolchain");

    // --paths has no base version to compare.
    let (d, _) = history(
        "paths-mode",
        CFG_ON,
        &[(".github/workflows/ci.yml", ci())],
        &[],
    );
    let o = Command::new(BIN)
        .args([
            "classify",
            "--config",
            "sieve.toml",
            "--paths",
            ".github/workflows/ci.yml",
        ])
        .env_remove("GATE_PROBE_FILES")
        .env_remove("SIEVE_FAULT")
        .current_dir(&d)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_run_all(&v, "needs the base version");
}

/// A base where `from` is already replaced by `to`, then an edit of `make b`.
fn edit_b_on(name: &str, from: &str, to: &str) -> serde_json::Value {
    let old = ci();
    assert!(old.contains(from), "fixture lacks {from:?}");
    let old = old.replacen(from, to, 1);
    let new = old.replacen("make b", "make b V=1", 1);
    let (d, base) = history(
        name,
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    classify_git(&d, &base)
}

// rivet: verifies REQ-030
#[test]
fn review_holes_select_everything() {
    // GitHub matches needs: case-insensitively: report needs JB is report
    // needs jb, and the exact-match closure would miss it.
    let v = edit_b_on("rev-needs-case", "needs: [jb]", "needs: [JB]");
    assert_run_all(&v, "not exactly a job id");
    // A dependent's step reads the changed job through index syntax.
    let v = edit_b_on(
        "rev-needs-index",
        "      - run: make report\n",
        "      - run: make report\n        if: ${{ needs['jb'].outputs.v == '1' }}\n",
    );
    assert_run_all(&v, "context other than github or runner");
    // A step switched off through job-level env, its if: untouched.
    let v = edit_b_on(
        "rev-env",
        "      - run: make b\n",
        "      - run: make b\n        if: ENV.RUN_B == 'true'\n",
    );
    assert_run_all(&v, "context other than github or runner");
    // A step if: on github or a status function stays scoped.
    let v = edit_b_on(
        "rev-github-if",
        "      - run: make b\n",
        "      - run: make b\n        if: github.event_name == 'pull_request'\n      - run: report\n        if: failure()\n",
    );
    assert_scoped(&v, &["b"]);

    // Swapping commands across a continue-on-error step keeps the list of
    // step if:/continue-on-error values but neuters the job.
    let two = "      - run: make b\n      - run: make lint-b\n        continue-on-error: true\n";
    let old = ci().replacen("      - run: make b\n", two, 1);
    let new = old.replacen(
        two,
        "      - run: make lint-b\n      - run: make b\n        continue-on-error: true\n",
        1,
    );
    let (d, base) = history(
        "rev-coe-swap",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    assert_run_all(
        &classify_git(&d, &base),
        "a step's `if:` or `continue-on-error:`",
    );
}

// rivet: verifies REQ-030
#[test]
fn a_dependent_with_an_expression_matrix_selects_everything() {
    // report's matrix comes from jb's output: an edit to jb can empty it.
    let v = edit_b_on(
        "rev-matrix-expr",
        "  report:\n    needs: [jb]\n",
        "  report:\n    needs: [jb]\n    strategy:\n      matrix:\n        x: ${{ fromJSON(needs.jb.outputs.m) }}\n",
    );
    assert_run_all(&v, "strategy computed from an expression");
    // A literal matrix stays scoped.
    let v = edit_b_on(
        "rev-matrix-literal",
        "  report:\n    needs: [jb]\n",
        "  report:\n    needs: [jb]\n    strategy:\n      matrix:\n        x: [1, 2]\n",
    );
    assert_scoped(&v, &["b"]);
}

// rivet: verifies REQ-030
#[test]
fn a_checkout_without_the_workflow_blob_selects_everything_and_says_why() {
    // A blobless (filter=blob:none) checkout: the workflow's blob at the
    // merge base is not in the object store (sieve#27 C, found by jig).
    let old = ci();
    let new = old.replacen("make a", "make a2", 1);
    let (d, base) = history(
        "blobless",
        CFG_ON,
        &[(".github/workflows/ci.yml", old)],
        &[(".github/workflows/ci.yml", Some(new))],
    );
    let oid = git(
        &d,
        &["rev-parse", &format!("{base}:.github/workflows/ci.yml")],
    );
    fs::remove_file(d.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
    let v = classify_git(&d, &base);
    assert_run_all(&v, "needs the history with blobs");
}
