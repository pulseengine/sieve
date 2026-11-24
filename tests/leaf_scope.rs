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

// ── sieve#27 A: file-wide selection for files without a classifier ─────────

/// A workflow with no classifier and ungated jobs (`extra_on` adds triggers,
/// `top` adds top-level keys, `job` replaces the job body).
fn plain(extra_on: &str, top: &str, job: &str) -> String {
    format!("name: plain\non:\n  pull_request:\n  merge_group:\n{extra_on}{top}jobs:\n{job}")
}

const PLAIN_JOB: &str = "  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make\n";

fn edit_files(
    name: &str,
    before: &[(&str, String)],
    after: &[(&str, Option<String>)],
) -> serde_json::Value {
    let (d, base) = history(name, CFG_ON, before, after);
    classify_git(&d, &base)
}

fn assert_file_wide(v: &serde_json::Value, want: &[&str]) {
    // without the file-wide rule each of these runs everything
    assert_scoped(v, want);
}

// rivet: verifies REQ-035
#[test]
fn an_unscopable_edit_to_a_file_without_a_classifier_selects_nothing() {
    let p = ".github/workflows/plain.yml";
    // outside `jobs:` (top-level env)
    let v = edit_files(
        "fw-env",
        &[(p, plain("", "", PLAIN_JOB))],
        &[(p, Some(plain("", "env:\n  X: 1\n", PLAIN_JOB)))],
    );
    assert_file_wide(&v, &[]);
    // a step `if:` on the github context, changed
    let gated = |c: &str| {
        format!("  build:\n    runs-on: ubuntu-latest\n    steps:\n      - id: s\n        run: make\n      - if: {c}\n        run: make more\n")
    };
    let v = edit_files(
        "fw-step-if",
        &[(
            p,
            plain("", "", &gated("github.event_name == 'pull_request'")),
        )],
        &[(
            p,
            Some(plain("", "", &gated("github.ref == 'refs/heads/main'"))),
        )],
    );
    assert_file_wide(&v, &[]);
    // an added file
    let v = edit_files("fw-added", &[], &[(p, Some(plain("", "", PLAIN_JOB)))]);
    assert_file_wide(&v, &[]);
    // combined with a code path
    let v = edit_files(
        "fw-with-code",
        &[(p, plain("", "", PLAIN_JOB))],
        &[
            (p, Some(plain("", "env:\n  X: 1\n", PLAIN_JOB))),
            ("a/x", Some("2".into())),
        ],
    );
    assert_file_wide(&v, &["a"]);
}

/// (name, files before, files after, expected reason)
type Control<'a> = (
    &'a str,
    Vec<(&'a str, String)>,
    Vec<(&'a str, Option<String>)>,
    &'a str,
);

// rivet: verifies REQ-035
#[test]
fn file_wide_selection_keeps_every_cross_file_and_classifier_case_running_everything() {
    let p = ".github/workflows/plain.yml";
    let env_edit = |before: String, after: String| (vec![(p, before)], vec![(p, Some(after))]);
    let cases: Vec<Control> = vec![
        // the classifier's own file: an outside-jobs edit
        {
            let (b, a) = env_edit(ci(), ci().replacen("jobs:\n", "env:\n  X: 1\njobs:\n", 1));
            ("fw-classifier-file", b, a, "hosts a classifier job")
        },
        // a reusable workflow
        {
            let (b, a) = env_edit(
                plain("  workflow_call:\n", "", PLAIN_JOB),
                plain("  workflow_call:\n", "env:\n  X: 1\n", PLAIN_JOB),
            );
            ("fw-reusable", b, a, "reusable workflow")
        },
        // triggered by workflow_run
        {
            let (b, a) = env_edit(
                plain(
                    "  workflow_run:\n    workflows: [ci]\n    types: [completed]\n",
                    "",
                    PLAIN_JOB,
                ),
                plain(
                    "  workflow_run:\n    workflows: [ci]\n    types: [completed]\n",
                    "env:\n  X: 1\n",
                    PLAIN_JOB,
                ),
            );
            ("fw-workflow-run", b, a, "triggered by `workflow_run`")
        },
        // another workflow runs on workflow_run of this one (by name)
        {
            let follower = "name: follow\non:\n  workflow_run:\n    workflows: [plain]\n    types: [completed]\njobs:\n  f:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make\n".to_string();
            (
                "fw-followed",
                vec![
                    (p, plain("", "", PLAIN_JOB)),
                    (".github/workflows/follow.yml", follower),
                ],
                vec![(p, Some(plain("", "env:\n  X: 1\n", PLAIN_JOB)))],
                "runs on `workflow_run` of",
            )
        },
        // a merge key
        {
            let (b, a) = env_edit(
                plain("", "", PLAIN_JOB),
                plain("", "defaults: &d\n  run:\n    shell: bash\n", "  build:\n    <<: *d\n    runs-on: ubuntu-latest\n    steps:\n      - run: make\n"),
            );
            ("fw-merge-key", b, a, "merge key")
        },
        // deleted
        (
            "fw-deleted",
            vec![(p, plain("", "", PLAIN_JOB))],
            vec![(p, None)],
            "is deleted",
        ),
        // a job that calls a reusable workflow
        {
            let caller = "  call:\n    uses: ./.github/workflows/lib.yml\n";
            let (b, a) = env_edit(plain("", "", caller), plain("", "env:\n  X: 1\n", caller));
            ("fw-caller", b, a, "calls a reusable workflow")
        },
        // an added file that hosts a classifier
        (
            "fw-added-classifier",
            vec![],
            vec![(p, Some(ci()))],
            "hosts a classifier job",
        ),
    ];
    let mut bad = vec![];
    for (name, before, after, why) in cases {
        let v = edit_files(name, &before, &after);
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains(why) {
            bad.push(format!(
                "{name}: run_all={} reason={reason:?} (want {why:?})",
                v["run_all"]
            ));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
}

// ── sieve#27 B: files GitHub's services read ───────────────────────────────

// rivet: verifies REQ-036
#[test]
fn a_github_service_file_no_workflow_names_runs_no_job() {
    let wf = (".github/workflows/plain.yml", plain("", "", PLAIN_JOB));
    for (name, p) in [
        ("svc-dependabot", ".github/dependabot.yml"),
        ("svc-codeowners", ".github/CODEOWNERS"),
        ("svc-funding", ".github/FUNDING.yml"),
        ("svc-issue-template", ".github/ISSUE_TEMPLATE/bug.md"),
        ("svc-pr-template", ".github/PULL_REQUEST_TEMPLATE.md"),
    ] {
        let v = edit_files(
            name,
            &[wf.clone(), (p, "x: 1\n".into())],
            &[(p, Some("x: 2\n".into()))],
        );
        assert_scoped(&v, &[]);
    }
}

// rivet: verifies REQ-036
#[test]
fn a_github_service_file_stays_run_all_when_named_listed_or_scoping_is_off() {
    let p = ".github/dependabot.yml";
    // a workflow that lints dependabot.yml
    let lint = plain("", "", "  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: yamllint .github/dependabot.yml\n");
    let v = edit_files(
        "svc-named",
        &[(".github/workflows/lint.yml", lint), (p, "x: 1\n".into())],
        &[(p, Some("x: 2\n".into()))],
    );
    assert_run_all(&v, "is CI, toolchain");
    // an action that reads the issue templates
    let v = edit_files(
        "svc-named-by-action",
        &[
            (".github/actions/triage/action.yml", "runs:\n  using: composite\n  steps:\n    - run: ls .github/ISSUE_TEMPLATE\n      shell: bash\n".into()),
            (".github/ISSUE_TEMPLATE/bug.md", "a\n".into()),
        ],
        &[(".github/ISSUE_TEMPLATE/bug.md", Some("b\n".into()))],
    );
    assert_run_all(&v, "is CI, toolchain");
    // a .github file outside the list
    let v = edit_files(
        "svc-release-notes",
        &[(".github/release-notes/v1.md", "a\n".into())],
        &[(".github/release-notes/v1.md", Some("b\n".into()))],
    );
    assert_run_all(&v, "is CI, toolchain");
    // the adopter lists it in [run_all]
    let cfg = format!("{CFG_ON}[run_all]\nfiles = [\".github/dependabot.yml\"]\n");
    let (d, base) = history(
        "svc-listed",
        &cfg,
        &[(p, "x: 1\n".into())],
        &[(p, Some("x: 2\n".into()))],
    );
    assert_eq!(classify_git(&d, &base)["run_all"], true);
    // scoping off
    let (d, base) = history(
        "svc-off",
        CFG_OFF,
        &[(p, "x: 1\n".into())],
        &[(p, Some("x: 2\n".into()))],
    );
    assert_eq!(classify_git(&d, &base)["run_all"], true);
}

// rivet: verifies REQ-036
#[test]
fn a_github_service_file_a_script_outside_github_reads_stays_run_all() {
    // security review: a workflow can run a script that reads CODEOWNERS
    let p = ".github/CODEOWNERS";
    let v = edit_files(
        "svc-named-by-script",
        &[
            (
                "scripts/owners.sh",
                "grep -q team .github/CODEOWNERS\n".into(),
            ),
            (p, "* @a\n".into()),
        ],
        &[(p, Some("* @b\n".into()))],
    );
    assert_run_all(&v, "is CI, toolchain");
    // a markdown file naming it counts too: an mdBook include reads it
    let v = edit_files(
        "svc-named-by-markdown",
        &[
            (
                "book/src/contributing.md",
                "{{#include ../../.github/CODEOWNERS}}\n".into(),
            ),
            (p, "* @a\n".into()),
        ],
        &[(p, Some("* @b\n".into()))],
    );
    assert_run_all(&v, "is CI, toolchain");
}

// ── security review of sieve#27 ────────────────────────────────────────────

/// lint.yml: a classifier and a job gated on class b that lints the repo.
fn lint_wf() -> String {
    format!(
        "name: lint\non:\n  pull_request:\n  merge_group:\njobs:\n{CLASSIFIER}  lint:\n    needs: changes\n    if: {g}\n    runs-on: ubuntu-latest\n    steps:\n      - run: yamllint . && actionlint\n",
        g = gate("b")
    )
}

// rivet: verifies REQ-035
#[test]
fn a_classifier_sieve_does_not_recognise_keeps_file_wide_off() {
    // the classifier runs through a wrapper script; ja is still gated on it
    let wf = |cmd: &str| {
        format!(
            "name: w\non:\n  pull_request:\n  merge_group:\njobs:\n  changes:\n    runs-on: ubuntu-latest\n    outputs:\n      a: ${{{{ steps.c.outputs.a }}}}\n    steps:\n      - id: c\n        run: ./ci/classify.sh --config sieve.toml\n  ja:\n    needs: changes\n    if: {g}\n    runs-on: ubuntu-latest\n    steps:\n      - run: {cmd}\n",
            g = gate("a")
        )
    };
    let p = ".github/workflows/w.yml";
    let v = edit_files(
        "rv-wrapper-classifier",
        &[(p, wf("make a"))],
        &[(p, Some(wf("make a-broken")))],
    );
    assert_run_all(&v, "may gate it");
}

// rivet: verifies REQ-035
// rivet: verifies REQ-036
#[test]
fn a_scoped_github_edit_selects_the_whole_repo_linters_class() {
    let lint = (".github/workflows/lint.yml", lint_wf());
    // B: an invalid dependabot.yml must still be linted
    let p = ".github/dependabot.yml";
    let v = edit_files(
        "rv-lint-service",
        &[lint.clone(), (p, "x: 1\n".into())],
        &[(p, Some("x: [\n".into()))],
    );
    assert_scoped(&v, &["b"]);
    // A: an added workflow with no classifier
    let n = ".github/workflows/new.yml";
    let v = edit_files(
        "rv-lint-added",
        std::slice::from_ref(&lint),
        &[(n, Some(plain("", "", PLAIN_JOB)))],
    );
    assert_scoped(&v, &["b"]);
    // REQ-030: a step edit in ci.yml's a-gated job
    let v = edit_files(
        "rv-lint-step",
        &[lint, (".github/workflows/ci.yml", ci())],
        &[(
            ".github/workflows/ci.yml",
            Some(ci().replacen("make a", "make a2", 1)),
        )],
    );
    assert_scoped(&v, &["a", "b"]);
}

// rivet: verifies REQ-036
#[test]
fn a_service_file_claimed_by_name_selects_its_class_and_a_submodule_does_not_block() {
    let p = ".github/dependabot.yml";
    // a class claims it by name in sieve.toml, which does not count as a namer
    let cfg = "[class.a]\nhand = [\"a/\"]\n[class.b]\nhand = [\"b/\", \".github/dependabot.yml\"]\n[workflows]\nscope_leaf_edits = true\n";
    let (d, base) = history(
        "rv-claimed",
        cfg,
        &[(p, "x: 1\n".into())],
        &[(p, Some("x: 2\n".into()))],
    );
    assert_scoped(&classify_git(&d, &base), &["b"]);
    // a submodule (a gitlink whose directory is empty) is skipped by the scan
    let (d, _) = history("rv-submodule", CFG_ON, &[(p, "x: 1\n".into())], &[]);
    let head = git(&d, &["rev-parse", "HEAD"]);
    git(
        &d,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},vendor/sub"),
        ],
    );
    fs::create_dir_all(d.join("vendor/sub")).unwrap();
    git(&d, &["commit", "-q", "-m", "submodule"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    fs::write(d.join(p), "x: 2\n").unwrap();
    git(&d, &["commit", "-q", "-am", "edit"]);
    let v = classify_git(&d, &base);
    assert_scoped(&v, &[]);
}

// rivet: verifies REQ-035
#[test]
fn a_gate_passed_through_env_keeps_file_wide_off() {
    // the wrapper classifier's output reaches a step through job env (e6)
    let wf = |cmd: &str| {
        format!(
            "name: w\non:\n  pull_request:\n  merge_group:\njobs:\n  changes:\n    runs-on: ubuntu-latest\n    outputs:\n      a: ${{{{ steps.c.outputs.a }}}}\n    steps:\n      - id: c\n        run: ./ci/classify.sh\n  ja:\n    needs: changes\n    runs-on: ubuntu-latest\n    env:\n      A: ${{{{ needs.changes.outputs.a }}}}\n    steps:\n      - if: env.A != 'false'\n        run: {cmd}\n"
        )
    };
    let p = ".github/workflows/w.yml";
    let v = edit_files(
        "rv2-env-gate",
        &[(p, wf("make a"))],
        &[(p, Some(wf("make a-broken")))],
    );
    assert_eq!(v["run_all"], true, "{v}");
    // the same gate inside `run:` with no step `if:` (only the job reading
    // `needs` shows it): `[ "$A" = false ] && exit 0` before the work
    let wf2 = |cmd: &str| {
        wf(cmd).replace(
            "      - if: env.A != 'false'\n        run: ",
            "      - run: '[ \"$A\" = false ] && exit 0; ",
        ) + ""
    };
    let v = edit_files(
        "rv2-env-run-gate",
        &[(p, wf2("make a'"))],
        &[(p, Some(wf2("make a-broken'")))],
    );
    assert_eq!(v["run_all"], true, "{v}");
    // the same job, with an edit outside `jobs:` (REQ-030 refuses; only the
    // file-wide rule's `needs` check keeps everything running)
    let v = edit_files(
        "rv2-env-run-gate-top",
        &[(p, wf2("make a'"))],
        &[(
            p,
            Some(wf2("make a'").replacen("jobs:\n", "env:\n  X: 1\njobs:\n", 1)),
        )],
    );
    assert_eq!(v["run_all"], true, "{v}");
}

/// lint.yml with a classifier and a job gated on class b whose steps are `steps`.
fn lint_wf_steps(steps: &str, job_if: Option<&str>, extra_jobs: &str) -> String {
    let ifl = job_if.map(|c| format!("    if: {c}\n")).unwrap_or_default();
    format!(
        "name: lint\non:\n  pull_request:\n  merge_group:\njobs:\n{CLASSIFIER}{extra_jobs}  lint:\n    needs: changes\n{ifl}    runs-on: ubuntu-latest\n    steps:\n{steps}"
    )
}

// rivet: verifies REQ-035
#[test]
fn linter_detection_covers_more_tools_composite_actions_and_gated_ancestors() {
    let n = ".github/workflows/new.yml";
    let added = |name: &str, files: Vec<(&str, String)>| {
        edit_files(name, &files, &[(n, Some(plain("", "", PLAIN_JOB)))])
    };
    let gb = gate("b");
    // e10: typos and reuse
    let v = added(
        "rv2-typos",
        vec![(
            ".github/workflows/lint.yml",
            lint_wf_steps("      - run: typos && reuse lint\n", Some(&gb), ""),
        )],
    );
    assert_scoped(&v, &["b"]);
    // e13: a linter inside a local composite action
    let v = added(
        "rv2-composite",
        vec![
            (
                ".github/workflows/lint.yml",
                lint_wf_steps("      - uses: ./.github/actions/lint\n", Some(&gb), ""),
            ),
            (
                ".github/actions/lint/action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: actionlint\n      shell: bash\n"
                    .into(),
            ),
        ],
    );
    assert_scoped(&v, &["b"]);
    // a linter job with no if: of its own, needing a b-gated job
    let gated = format!(
        "  prep:\n    needs: changes\n    if: {gb}\n    runs-on: ubuntu-latest\n    steps:\n      - run: make prep\n"
    );
    let wf = lint_wf_steps("      - run: actionlint\n", None, &gated).replacen(
        "  lint:\n    needs: changes\n",
        "  lint:\n    needs: [prep]\n",
        1,
    );
    let v = added("rv2-ancestor", vec![(".github/workflows/lint.yml", wf)]);
    assert_scoped(&v, &["b"]);
    // e12: a linter gated by an `if:` that is not the classifier form
    let v = added(
        "rv2-noncanonical",
        vec![(
            ".github/workflows/lint.yml",
            lint_wf_steps(
                "      - run: actionlint\n",
                Some("needs.changes.outputs.b == 'true'"),
                "",
            ),
        )],
    );
    assert_run_all(&v, "not the classifier form");
}

// rivet: verifies REQ-036
#[cfg(unix)]
#[test]
fn a_symlink_whose_target_names_a_service_file_keeps_it_run_all() {
    let p = ".github/CODEOWNERS";
    let (d, _) = history("rv2-symlink", CFG_ON, &[(p, "* @a\n".into())], &[]);
    std::os::unix::fs::symlink(".github/CODEOWNERS", d.join("docs-owners")).unwrap();
    git(&d, &["add", "docs-owners"]);
    git(&d, &["commit", "-q", "-m", "link"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    fs::write(d.join(p), "* @b\n").unwrap();
    git(&d, &["commit", "-q", "-am", "edit"]);
    assert_eq!(classify_git(&d, &base)["run_all"], true);
}

// rivet: verifies REQ-035
#[test]
fn linter_detection_follows_local_reusable_workflows_and_fails_closed() {
    let n = ".github/workflows/new.yml";
    let added = |name: &str, files: Vec<(&str, String)>| {
        edit_files(name, &files, &[(n, Some(plain("", "", PLAIN_JOB)))])
    };
    let gb = gate("b");
    let caller = format!(
        "name: ci2\non:\n  pull_request:\n  merge_group:\njobs:\n{CLASSIFIER}  lint:\n    needs: changes\n    if: {gb}\n    uses: ./.github/workflows/lint-lib.yml\n"
    );
    let lib = "name: lint-lib\non:\n  workflow_call:\njobs:\n  l:\n    runs-on: ubuntu-latest\n    steps:\n      - run: yamllint . && actionlint\n".to_string();
    // e14/e15: the linter runs inside a local reusable workflow
    let v = added(
        "rv3-reusable",
        vec![
            (".github/workflows/ci2.yml", caller.clone()),
            (".github/workflows/lint-lib.yml", lib),
        ],
    );
    assert_scoped(&v, &["b"]);
    // a local `uses:` that resolves to nothing tracked
    let v = added(
        "rv3-unresolved",
        vec![(".github/workflows/ci2.yml", caller)],
    );
    assert_run_all(&v, "names no tracked action or workflow");
    // a linter step behind its own step-level gate
    let stepgated = lint_wf_steps(
        "      - if: needs.changes.outputs.b != 'false'\n        run: actionlint\n",
        None,
        "",
    );
    let v = added(
        "rv3-step-gated",
        vec![(".github/workflows/lint.yml", stepgated)],
    );
    assert_run_all(&v, "cannot map to a class");
}

// rivet: verifies REQ-035
#[test]
fn a_job_chain_in_a_file_without_a_classifier_keeps_file_wide_off() {
    // e16: a gate passed as an artifact needs no `needs` text in the job body
    let chain = |cmd: &str| {
        format!("name: c\non:\n  pull_request:\n  merge_group:\njobs:\n  first:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make verdict\n  second:\n    needs: first\n    runs-on: ubuntu-latest\n    steps:\n      - run: {cmd}\n")
    };
    let p = ".github/workflows/c.yml";
    let v = edit_files(
        "rv3-chain",
        &[(p, chain("make b"))],
        &[(
            p,
            Some(chain("make b").replacen("jobs:\n", "env:\n  X: 1\njobs:\n", 1)),
        )],
    );
    assert_run_all(&v, "needs another job");
}

// rivet: verifies REQ-035
#[test]
fn linter_detection_reads_uses_as_yaml_not_text() {
    let n = ".github/workflows/new.yml";
    let gb = gate("b");
    let wf = |uses: &str| lint_wf_steps(&format!("      - uses: {uses}\n"), Some(&gb), "");
    // i7: a nested call with a quoted key
    let v = edit_files(
        "rv4-quoted",
        &[
            (".github/workflows/lint.yml", wf("./.github/actions/outer")),
            (
                ".github/actions/outer/action.yml",
                "runs:\n  using: composite\n  steps:\n    - \"uses\": ./.github/actions/l\n".into(),
            ),
            (
                ".github/actions/l/action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: yamllint .\n      shell: bash\n"
                    .into(),
            ),
        ],
        &[(n, Some(plain("", "", PLAIN_JOB)))],
    );
    assert_scoped(&v, &["b"]);
    // the repository-root action (`uses: ./`)
    let v = edit_files(
        "rv4-root-action",
        &[
            (".github/workflows/lint.yml", wf("./")),
            (
                "action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: actionlint\n      shell: bash\n"
                    .into(),
            ),
        ],
        &[(n, Some(plain("", "", PLAIN_JOB)))],
    );
    assert_scoped(&v, &["b"]);
}

// ── sieve#36, sieve#37 (gale's L18) ───────────────────────────────────────

/// renode.yml: gale's shape, a checkout under `path:` then a local action
/// through that path (`steps` replaces the job's steps).
fn renode_wf(steps: &str) -> String {
    format!("name: renode\non:\n  pull_request:\n  merge_group:\njobs:\n  renode:\n    runs-on: ubuntu-24.04\n    steps:\n{steps}")
}

const CHECKOUT_GALE: &str =
    "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1\n        with:\n          path: gale\n";
const USE_RENODE: &str =
    "      - uses: ./gale/.github/actions/setup-renode\n      - run: echo ok\n";

fn renode_case(name: &str, steps: &str) -> serde_json::Value {
    let p = ".github/dependabot.yml";
    edit_files(
        name,
        &[
            (".github/workflows/renode.yml", renode_wf(steps)),
            (
                ".github/actions/setup-renode/action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: echo renode\n      shell: bash\n"
                    .into(),
            ),
            (p, "x: 1\n".into()),
        ],
        &[(p, Some("x: 2\n".into()))],
    )
}

// rivet: verifies REQ-035
#[test]
fn a_local_action_under_a_same_repo_checkout_path_resolves() {
    let v = renode_case("i36-gale", &format!("{CHECKOUT_GALE}{USE_RENODE}"));
    assert_scoped(&v, &[]);
    // `repository: ${{ github.repository }}` is the same repository
    let same = "      - uses: actions/checkout@v4\n        with:\n          repository: ${{ github.repository }}\n          path: gale\n";
    let v = renode_case("i36-same-repo", &format!("{same}{USE_RENODE}"));
    assert_scoped(&v, &[]);
}

// rivet: verifies REQ-035
#[test]
fn a_local_action_under_an_unprovable_checkout_path_runs_everything() {
    let other = "      - uses: actions/checkout@v4\n        with:\n          repository: other/repo\n          path: gale\n";
    let expr =
        "      - uses: actions/checkout@v4\n        with:\n          path: ${{ matrix.p }}\n";
    let cases = [
        (
            "i36-other-repo",
            format!("{other}{USE_RENODE}"),
            "another repository",
        ),
        (
            "i36-path-expr",
            format!("{expr}{USE_RENODE}"),
            "cannot model",
        ),
        (
            "i36-use-before",
            format!("{USE_RENODE}{CHECKOUT_GALE}"),
            "names no tracked action",
        ),
        (
            "i36-overwritten",
            format!("{CHECKOUT_GALE}{other}{USE_RENODE}"),
            "another repository",
        ),
    ];
    let mut bad = vec![];
    for (name, steps, why) in cases {
        let v = renode_case(name, &steps);
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains(why) {
            bad.push(format!(
                "{name}: run_all={} reason={reason:?} (want {why:?})",
                v["run_all"]
            ));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-030
#[test]
fn the_word_needs_in_a_run_comment_is_not_a_needs_read() {
    let commented = |cmd: &str| {
        ci().replacen(
            "      - run: make a\n",
            &format!("      - run: |\n          # prove curl exists before anything needs it\n          {cmd}\n"),
            1,
        )
    };
    let p = ".github/workflows/ci.yml";
    let v = edit_files(
        "i37-comment",
        &[(p, commented("make a"))],
        &[(p, Some(commented("make a2")))],
    );
    assert_scoped(&v, &["a"]);
    // the same job reading needs through a step env still refuses
    let reads = |cmd: &str| {
        commented(cmd).replacen(
            "      - run: |\n          # prove",
            "      - env:\n          A: ${{ needs.changes.outputs.a }}\n        run: |\n          # prove",
            1,
        )
    };
    let v = edit_files(
        "i37-env-read",
        &[(p, reads("make a"))],
        &[(p, Some(reads("make a2")))],
    );
    assert_run_all(&v, "reads `needs` outside its `if:`");
}

// rivet: verifies REQ-035
#[test]
fn checkout_resolution_fails_closed_on_forks_refs_rewrites_and_dotdot() {
    let with = |extra: &str| {
        format!("      - uses: actions/checkout@v4\n        with:\n          path: gale\n{extra}")
    };
    let cases = [
        (
            "i36b-fork",
            format!("      - uses: my/checkout@v4\n        with:\n          path: gale\n{USE_RENODE}"),
            "cannot model",
        ),
        ("i36b-ref", format!("{}{USE_RENODE}", with("          ref: main\n")), "another repository or ref"),
        (
            "i36b-rewrite",
            format!("{CHECKOUT_GALE}      - run: rm -rf gale && git clone https://example.invalid/x gale\n{USE_RENODE}"),
            "another repository or ref",
        ),
        (
            "i36b-dotdot-path",
            format!("      - uses: actions/checkout@v4\n        with:\n          path: x/../gale\n{USE_RENODE}"),
            "cannot model",
        ),
        (
            "i36b-dotdot-uses",
            format!("{CHECKOUT_GALE}      - uses: ./gale/../gale/.github/actions/setup-renode\n"),
            "`..` segment",
        ),
    ];
    let mut bad = vec![];
    for (name, steps, why) in cases {
        let v = renode_case(name, &steps);
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains(why) {
            bad.push(format!(
                "{name}: run_all={} reason={reason:?} (want {why:?})",
                v["run_all"]
            ));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
    // the action name is matched without case
    let v = renode_case(
        "i36b-case",
        &format!(
            "      - uses: Actions/Checkout@v4\n        with:\n          path: gale\n{USE_RENODE}"
        ),
    );
    assert_scoped(&v, &[]);
}

// rivet: verifies REQ-030
#[test]
fn a_closing_brace_pair_inside_a_string_literal_does_not_end_the_expression() {
    // reviewer r4/r5: `}}` inside '...' in format() is not the expression end
    let gate = |cmd: &str| {
        ci().replacen(
            "      - run: make a\n",
            &format!("      - run: |\n          A='${{{{ format('{{{{\"a\":{{0}}}}}}', needs.changes.outputs.a) }}}}'; echo \"$A\" | grep -q true || exit 0\n          {cmd}\n"),
            1,
        )
    };
    let p = ".github/workflows/ci.yml";
    let v = edit_files(
        "i37b-format",
        &[(p, gate("make a"))],
        &[(p, Some(gate("make a2")))],
    );
    assert_run_all(&v, "reads `needs` outside its `if:`");
}

// rivet: verifies REQ-035
#[test]
fn checkout_resolution_reads_inputs_without_case_and_sees_other_rewriters() {
    let cases = [
        (
            "i36c-ref-case",
            format!("      - uses: actions/checkout@v4\n        with:\n          Path: gale\n          Ref: main\n{USE_RENODE}"),
            "another repository or ref",
        ),
        (
            "i36c-workdir",
            format!("{CHECKOUT_GALE}      - working-directory: gale\n        run: git checkout other-branch\n{USE_RENODE}"),
            "another repository or ref",
        ),
        (
            "i36c-other-action",
            format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          dest: gale/\n{USE_RENODE}"),
            "another repository or ref",
        ),
    ];
    let mut bad = vec![];
    for (name, steps, why) in cases {
        let v = renode_case(name, &steps);
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains(why) {
            bad.push(format!(
                "{name}: run_all={} reason={reason:?} (want {why:?})",
                v["run_all"]
            ));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
    // an unrelated input that merely contains the name does not count
    let v = renode_case(
        "i36c-cache-key",
        &format!("{CHECKOUT_GALE}      - uses: actions/cache@v4\n        with:\n          path: ~/.cache/renode\n          key: gale-renode\n{USE_RENODE}"),
    );
    assert_scoped(&v, &[]);
}

// rivet: verifies REQ-035
#[test]
fn checkout_resolution_sees_defaults_parents_and_duplicate_inputs() {
    let git_step = "      - run: git checkout other-branch\n";
    let job_defaults = format!("{CHECKOUT_GALE}{git_step}{USE_RENODE}");
    let cases: Vec<(&str, String, &str)> = vec![
        ("i36d-dest-dot", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          dest: .\n{USE_RENODE}"), "another repository or ref"),
        ("i36d-dest-dotdot", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          dest: x/../gale\n{USE_RENODE}"), "another repository or ref"),
        ("i36d-dup-keys", format!("      - uses: actions/checkout@v4\n        with:\n          Path: gale\n          path: gale\n{USE_RENODE}"), "cannot model"),
    ];
    let mut bad = vec![];
    for (name, steps, why) in cases {
        let v = renode_case(name, &steps);
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains(why) {
            bad.push(format!(
                "{name}: run_all={} reason={reason:?} (want {why:?})",
                v["run_all"]
            ));
        }
    }
    // job- and workflow-level `defaults.run.working-directory: gale`
    for (name, wf) in [
        (
            "i36d-job-defaults",
            renode_wf(&job_defaults).replacen(
                "    runs-on: ubuntu-24.04\n",
                "    runs-on: ubuntu-24.04\n    defaults:\n      run:\n        working-directory: gale\n",
                1,
            ),
        ),
        (
            "i36d-wf-defaults",
            renode_wf(&job_defaults).replacen(
                "jobs:\n",
                "defaults:\n  run:\n    working-directory: gale\njobs:\n",
                1,
            ),
        ),
    ] {
        let p = ".github/dependabot.yml";
        let v = edit_files(
            name,
            &[
                (".github/workflows/renode.yml", wf),
                (
                    ".github/actions/setup-renode/action.yml",
                    "runs:\n  using: composite\n  steps:\n    - run: echo renode\n      shell: bash\n".into(),
                ),
                (p, "x: 1\n".into()),
            ],
            &[(p, Some("x: 2\n".into()))],
        );
        let reason = v["reason"].as_str().unwrap_or("");
        if v["run_all"] != true || !reason.contains("another repository or ref") {
            bad.push(format!("{name}: run_all={} reason={reason:?}", v["run_all"]));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
}

// rivet: verifies REQ-035
#[test]
fn checkout_resolution_handles_case_backslashes_and_more_path_keys() {
    let cases = [
        ("i36e-backslash", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          dest: 'GALE\\sub'\n{USE_RENODE}")),
        ("i36e-drive", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          dest: 'C:\\w'\n{USE_RENODE}")),
        ("i36e-cwd-key", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          cwd: .\n{USE_RENODE}")),
        ("i36e-suffix-key", format!("{CHECKOUT_GALE}      - uses: some/unpack@v1\n        with:\n          extract_dir: .\n{USE_RENODE}")),
        ("i36e-run-case", format!("{CHECKOUT_GALE}      - run: rm -rf GALE\n{USE_RENODE}")),
    ];
    let mut bad = vec![];
    for (name, steps) in cases {
        let v = renode_case(name, &steps);
        if v["run_all"] != true {
            bad.push(format!(
                "{name}: run_all={} reason={:?}",
                v["run_all"], v["reason"]
            ));
        }
    }
    assert!(bad.is_empty(), "controls not holding:\n{}", bad.join("\n"));
}
