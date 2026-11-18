//! `sieve check-workflows`: assert every required status check will actually
//! be produced on every pull request, and can be skipped only through the
//! classifier. Exit 0 pass, 1 a gate failed, 2 usage, 3 refused.
//!
//! A required context that never reports leaves a PR BLOCKED on a check that
//! will never arrive; a required job skipped by anything but the classifier
//! reports "skipped", which branch protection counts as passing.
//!
//!  (a) trigger state: the workflow hosting each required context triggers on
//!      `pull_request` with no `paths`/`paths-ignore` KEY (an empty one parses
//!      as null: ambiguous, and that is the point), no branch restriction that
//!      excludes main, no `types:` dropping a default activity, and also on
//!      `merge_group` (the queue waits on its own run).
//!  (b) a required job is skipped only by the classifier: its `if:` is absent
//!      or exactly
//!        !cancelled() && (needs.<C>.result != 'success' || needs.<C>.outputs.<o> != 'false')
//!      with <C> the job that runs `sieve classify ... --format github-output`,
//!      wired exactly, and <o> one of the outputs the HAND-MAINTAINED
//!      `[workflows.job_outputs]` map of sieve.toml gives that job; plus the
//!      hardening listed at `job_skip_state`.
//!  (c) the classifier fails OPEN: sieve's own classification, in process, on
//!      push, merge_group, no base, an undiffable base and an empty diff
//!      selects every class (the check-graph `fail-open` control).
//!
//! What this does NOT prove: that `[workflows.job_outputs]` maps each job to
//! the output that really covers its inputs, or that the classes' input sets
//! are complete. That map is HAND-MAINTAINED; the false-skip controls test
//! its content. Nor does it read the real branch protection (needs admin
//! rights): that direction is reported as UNGUARDED.

use crate::config::Config;
use regex::Regex;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use yaml_rust2::{Yaml, YamlLoader};

const DEFAULT_WORKFLOWS: &str = ".github/workflows";
const DEFAULT_REQUIRED: &str = ".github/required-contexts.txt";
/// A pull_request event fires on these activity types unless `types:` overrides it.
const DEFAULT_PR_TYPES: &[&str] = &["opened", "synchronize", "reopened"];
/// Read by the classifier; a workflow or job that sets one overrides the event.
const CLASSIFIER_ENV_KEYS: &[&str] = &[
    "EVENT_NAME",
    "BASE_SHA",
    "GATE_PROBE_FILES",
    "GITHUB_EVENT_NAME",
];
const CLASSIFIER_ENV: &[(&str, &str)] = &[
    ("EVENT_NAME", "${{ github.event_name }}"),
    ("BASE_SHA", "${{ github.event.pull_request.base.sha }}"),
];

// ---------------------------------------------------------------- YAML access

fn key_is(k: &Yaml, name: &str) -> bool {
    matches!(k, Yaml::String(s) if s == name)
}

fn get<'a>(y: &'a Yaml, name: &str) -> Option<&'a Yaml> {
    match y {
        Yaml::Hash(h) => h.iter().find(|(k, _)| key_is(k, name)).map(|(_, v)| v),
        _ => None,
    }
}

fn has(y: &Yaml, name: &str) -> bool {
    get(y, name).is_some()
}

/// `x.get(k)` where a null counts as absent (Python's `is not None`).
fn get_nn<'a>(y: &'a Yaml, name: &str) -> Option<&'a Yaml> {
    get(y, name).filter(|v| !matches!(v, Yaml::Null))
}

fn entries(y: Option<&Yaml>) -> Vec<(String, &Yaml)> {
    match y {
        Some(Yaml::Hash(h)) => h.iter().map(|(k, v)| (py_str(k), v)).collect(),
        _ => vec![],
    }
}

fn list(y: Option<&Yaml>) -> Vec<&Yaml> {
    match y {
        Some(Yaml::Array(a)) => a.iter().collect(),
        _ => vec![],
    }
}

/// Python `str()` of a scalar, close enough for messages and regex tests.
fn py_str(y: &Yaml) -> String {
    match y {
        Yaml::String(s) | Yaml::Real(s) => s.clone(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(true) => "True".into(),
        Yaml::Boolean(false) => "False".into(),
        Yaml::Null => "None".into(),
        other => format!("{other:?}"),
    }
}

/// Every string anywhere under `y`, newline-joined (Python `json.dumps` search).
fn flatten(y: &Yaml, out: &mut String) {
    match y {
        Yaml::Array(a) => a.iter().for_each(|v| flatten(v, out)),
        Yaml::Hash(h) => h.iter().for_each(|(k, v)| {
            flatten(k, out);
            flatten(v, out);
        }),
        other => {
            out.push_str(&py_str(other));
            out.push('\n');
        }
    }
}

fn truthy(y: &Yaml) -> bool {
    match y {
        Yaml::Null | Yaml::BadValue | Yaml::Boolean(false) => false,
        Yaml::Integer(i) => *i != 0,
        Yaml::String(s) | Yaml::Real(s) => !s.is_empty(),
        Yaml::Array(a) => !a.is_empty(),
        Yaml::Hash(h) => !h.is_empty(),
        _ => true,
    }
}

/// The string members of a YAML scalar-or-list (a scalar is a one-element list).
fn members(y: &Yaml) -> Vec<String> {
    match y {
        Yaml::Array(a) => a.iter().map(py_str).collect(),
        other => vec![py_str(other)],
    }
}

fn show(y: &Yaml) -> String {
    members(y).join(", ")
}

// ------------------------------------------------------------- (a) triggers

type Problem = (&'static str, String);

/// Why (or whether) this workflow reports on every PR against main and in the
/// merge queue. `None` is fine.
fn trigger_state(wf: &Yaml) -> Option<Problem> {
    // YAML 1.1 parsers read the key `on` as boolean true; 1.2 ones as "on".
    let on = match wf {
        Yaml::Hash(h) => h
            .iter()
            .find(|(k, _)| matches!(k, Yaml::Boolean(true)))
            .map(|(_, v)| v),
        _ => None,
    }
    .or_else(|| get(wf, "on"));
    let no_mg = || {
        Some((
            "no-merge-group",
            "no merge_group trigger -- never reports in the merge queue".to_string(),
        ))
    };
    match on {
        Some(Yaml::String(s)) => {
            if s == "pull_request" {
                no_mg()
            } else {
                Some(("no-pr", format!("only `on: {s}`")))
            }
        }
        Some(Yaml::Array(a)) => {
            let names: Vec<String> = a.iter().map(py_str).collect();
            if !names.iter().any(|n| n == "pull_request") {
                Some(("no-pr", "no pull_request trigger".into()))
            } else if !names.iter().any(|n| n == "merge_group") {
                no_mg()
            } else {
                None
            }
        }
        Some(on @ Yaml::Hash(_)) => {
            let Some(pr) = get(on, "pull_request") else {
                return Some(("no-pr", "no pull_request trigger".into()));
            };
            // A bare `pull_request:` fires on everything; it still needs the queue trigger.
            if !matches!(pr, Yaml::Null) {
                if !matches!(pr, Yaml::Hash(_)) {
                    return Some(("no-pr", format!("unparseable pull_request: {pr:?}")));
                }
                // Presence of the KEY is what matters, not its value (gale#340).
                for k in ["paths", "paths-ignore"] {
                    if let Some(v) = get(pr, k) {
                        let mut detail = format!("`{k}:` present");
                        match v {
                            Yaml::Null => detail.push_str(
                                " with no entries (parses as null -- ambiguous, and that is the point)",
                            ),
                            Yaml::Array(a) => detail.push_str(&format!(" ({} pattern(s))", a.len())),
                            _ => {}
                        }
                        return Some(("paths-filter", detail));
                    }
                }
                if let Some(b) = get_nn(pr, "branches") {
                    if !members(b).iter().any(|m| m == "main") {
                        return Some((
                            "branch-mismatch",
                            format!("branches=[{}] excludes main", show(b)),
                        ));
                    }
                }
                if let Some(b) = get(pr, "branches-ignore").filter(|b| truthy(b)) {
                    return Some(("branch-mismatch", format!("branches-ignore=[{}]", show(b))));
                }
                if let Some(t) = get_nn(pr, "types") {
                    let have = members(t);
                    let missing: Vec<&str> = DEFAULT_PR_TYPES
                        .iter()
                        .copied()
                        .filter(|d| !have.iter().any(|h| h == d))
                        .collect();
                    if !missing.is_empty() {
                        return Some((
                            "types-restricted",
                            format!("types=[{}] drops {missing:?}", show(t)),
                        ));
                    }
                }
            }
            // The queue runs required checks on `merge_group`, whose only filter
            // is `types:` and whose only activity type is checks_requested.
            let Some(mg) = get(on, "merge_group") else {
                return no_mg();
            };
            if let Some(t) = get_nn(mg, "types") {
                if !members(t).iter().any(|m| m == "checks_requested") {
                    return Some((
                        "no-merge-group",
                        format!("merge_group types=[{}] drops checks_requested", show(t)),
                    ));
                }
            }
            None
        }
        _ => Some(("no-pr", "unparseable `on:`".into())),
    }
}

// ---------------------------------------------------------------- the index

struct Wf {
    file: String,
    doc: Yaml,
    state: Option<Problem>,
}

struct Hit {
    wf: usize,
    jid: String,
}

fn job_name(job: &Yaml, jid: &str) -> String {
    match get(job, "name") {
        Some(n) if truthy(n) => py_str(n),
        _ => jid.to_string(),
    }
}

// --------------------------------------------------- (b) skip only via sieve

fn needs_result() -> Regex {
    Regex::new(r"^needs\.([A-Za-z0-9_-]+)\.result\s*!=\s*'success'$").unwrap()
}
fn atom() -> Regex {
    Regex::new(r"^needs\.([A-Za-z0-9_-]+)\.outputs\.([A-Za-z0-9_-]+)\s*!=\s*'false'$").unwrap()
}
fn mentions_classifier(run: &str) -> bool {
    Regex::new(r"\bsieve\s+classify\b").unwrap().is_match(run)
}

fn run_of(st: &Yaml) -> String {
    get_nn(st, "run").map(py_str).unwrap_or_default()
}

/// {job id -> (classifier step, output names wired to that step)} for every
/// job with a step that mentions the classifier (exactness is checked apart).
fn classifier_jobs(wf: &Yaml) -> BTreeMap<String, (&Yaml, BTreeSet<String>)> {
    let mut out = BTreeMap::new();
    for (jid, job) in entries(get(wf, "jobs")) {
        for st in list(get(job, "steps")) {
            if !mentions_classifier(&run_of(st)) {
                continue;
            }
            let sid = get_nn(st, "id").map(py_str);
            let mut outs = BTreeSet::new();
            for (k, v) in entries(get(job, "outputs")) {
                if let Some(sid) = &sid {
                    let re = Regex::new(&format!(
                        r"^\$\{{\{{\s*steps\.{}\.outputs\.{}\s*\}}\}}$",
                        regex::escape(sid),
                        regex::escape(&k)
                    ))
                    .unwrap();
                    if re.is_match(py_str(v).trim()) {
                        outs.insert(k);
                    }
                }
            }
            out.insert(jid.clone(), (st, outs));
        }
    }
    out
}

/// Whether `run:` is exactly `sieve classify --config <this config> --base
/// "$BASE_SHA" --event "$EVENT_NAME" --format github-output` (flags in any
/// order, the program optionally given as a path ending in `sieve`).
fn run_problem(run: &str, cfg_path: &str) -> Option<String> {
    let want = format!(
        "sieve classify --config {cfg_path} --base \"$BASE_SHA\" --event \"$EVENT_NAME\" --format github-output"
    );
    let bad = || Some(format!("`run:` is {:?}, not exactly {want:?}", run.trim()));
    let run = run.trim();
    // Chaining, redirection, substitution, continuation and single quotes
    // cannot survive the token match below: each glues onto a token or adds an
    // unknown one, and only the exact flag values are accepted.
    if run.is_empty() {
        return bad();
    }
    let mut toks: Vec<String> = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut in_tok = false;
    for ch in run.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                in_tok = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_tok {
                    toks.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            c => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if quoted {
        return bad();
    }
    if in_tok {
        toks.push(cur);
    }
    let prog_ok = toks.first().is_some_and(|p| {
        p.rsplit('/').next() == Some("sieve")
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    });
    if !prog_ok || toks.get(1).map(String::as_str) != Some("classify") {
        return bad();
    }
    let norm = |p: &str| p.strip_prefix("./").unwrap_or(p).to_string();
    let (mut config, mut base, mut event, mut format) = (None, None, None, None);
    let mut i = 2;
    while i < toks.len() {
        let (flag, val) = (toks[i].as_str(), toks.get(i + 1));
        let slot = match flag {
            "--config" => &mut config,
            "--base" => &mut base,
            "--event" => &mut event,
            "--format" => &mut format,
            _ => return bad(),
        };
        let Some(v) = val else { return bad() };
        if slot.replace(v.clone()).is_some() {
            return bad();
        }
        i += 2;
    }
    let base_ok = matches!(base.as_deref(), Some("$BASE_SHA" | "${BASE_SHA}"));
    let event_ok = matches!(event.as_deref(), Some("$EVENT_NAME" | "${EVENT_NAME}"));
    let config_ok = config.as_deref().map(norm).as_deref() == Some(cfg_path);
    if base_ok && event_ok && config_ok && format.as_deref() == Some("github-output") {
        None
    } else {
        bad()
    }
}

/// [problems] with how classifier job `cjid` runs the classifier.
fn classifier_wiring(wf: &Yaml, cjid: &str, cfg_path: &str) -> Vec<String> {
    let jobs = entries(get(wf, "jobs"));
    let job = jobs
        .iter()
        .find(|(j, _)| j == cjid)
        .map(|(_, y)| *y)
        .unwrap_or(&Yaml::Null);
    let steps: Vec<&Yaml> = list(get(job, "steps"))
        .into_iter()
        .filter(|s| mentions_classifier(&run_of(s)))
        .collect();
    if steps.len() != 1 {
        return vec![format!(
            "{cjid}: {} steps mention `sieve classify`; exactly one must run it",
            steps.len()
        )];
    }
    let st = steps[0];
    let mut probs = vec![];
    if let Some(p) = run_problem(&run_of(st), cfg_path) {
        probs.push(format!("{cjid}: classifier step {p}"));
    }
    let env: BTreeMap<String, String> = entries(get(st, "env"))
        .into_iter()
        .map(|(k, v)| (k, py_str(v).trim().to_string()))
        .collect();
    let want: BTreeMap<String, String> = CLASSIFIER_ENV
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if env != want {
        probs.push(format!(
            "{cjid}: classifier step `env:` is {env:?}, not exactly {want:?}"
        ));
    }
    for k in ["if", "continue-on-error"] {
        if has(st, k) {
            probs.push(format!("{cjid}: classifier step has `{k}:`"));
        }
        if has(job, k) {
            probs.push(format!("{cjid}: classifier job has `{k}:`"));
        }
    }
    for (where_, e) in [("job", get(job, "env")), ("workflow", get(wf, "env"))] {
        let keys: BTreeSet<String> = entries(e).into_iter().map(|(k, _)| k).collect();
        let hit: Vec<&&str> = CLASSIFIER_ENV_KEYS
            .iter()
            .filter(|k| keys.contains(**k))
            .collect();
        if !hit.is_empty() {
            probs.push(format!(
                "{cjid}: {where_}-level env sets {hit:?}, which the classifier reads"
            ));
        }
    }
    if get_nn(st, "id").is_none() {
        probs.push(format!(
            "{cjid}: classifier step has no id, so no output can be wired to it"
        ));
    }
    probs
}

fn split_top(expr: &str, op: &str) -> Vec<String> {
    let chars: Vec<char> = expr.chars().collect();
    let op: Vec<char> = op.chars().collect();
    let (mut parts, mut depth, mut cur, mut i) = (vec![], 0i32, String::new(), 0);
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && chars[i..].starts_with(&op) {
            parts.push(cur.trim().to_string());
            cur.clear();
            i += op.len();
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }
    parts.push(cur.trim().to_string());
    parts
}

/// Remove ONE pair of parentheses enclosing the whole expression, if any.
fn strip_parens(e: &str) -> (String, bool) {
    let e = e.trim();
    if !(e.starts_with('(') && e.ends_with(')')) {
        return (e.to_string(), false);
    }
    let mut depth = 0i32;
    let n = e.chars().count();
    for (i, ch) in e.chars().enumerate() {
        depth += (ch == '(') as i32;
        depth -= (ch == ')') as i32;
        if depth == 0 && i < n - 1 {
            return (e.to_string(), false);
        }
    }
    (e[1..e.len() - 1].trim().to_string(), true)
}

/// (classifier job, outputs) if `cond` is exactly the accepted form, else why not.
fn parse_gate_if(cond: &str) -> Result<(String, Vec<String>), String> {
    let mut e = cond.trim();
    if e.starts_with("${{") && e.ends_with("}}") && e.len() >= 5 {
        e = e[3..e.len() - 2].trim();
    }
    let conj = split_top(e, "&&");
    if conj.len() != 2 || conj[0] != "!cancelled()" {
        return Err(format!(
            "`if: {cond}` is not `!cancelled() && (needs.<C>.result != 'success' || \
needs.<C>.outputs.<o> != 'false')`"
        ));
    }
    let (inner, had) = strip_parens(&conj[1]);
    if !had {
        return Err(format!(
            "`if:` second term `{}` is not the parenthesised classifier disjunction",
            conj[1]
        ));
    }
    let terms: Vec<String> = split_top(&inner, "||")
        .iter()
        .map(|t| strip_parens(t).0)
        .collect();
    let (res_re, atom_re) = (needs_result(), atom());
    let res: Vec<_> = terms.iter().map(|t| res_re.captures(t)).collect();
    let atoms: Vec<_> = terms.iter().map(|t| atom_re.captures(t)).collect();
    if res.iter().filter(|m| m.is_some()).count() != 1 {
        return Err(
            "`if:` lacks exactly one `needs.<C>.result != 'success'`: a classifier that \
wrote false and then failed would skip the job"
                .into(),
        );
    }
    for ((t, r), a) in terms.iter().zip(&res).zip(&atoms) {
        if r.is_none() && a.is_none() {
            return Err(format!(
                "`if:` term `{t}` can skip it for a reason other than the classifier"
            ));
        }
    }
    let cs: BTreeSet<String> = res
        .iter()
        .chain(atoms.iter())
        .flatten()
        .map(|m| m[1].to_string())
        .collect();
    if cs.len() != 1 {
        return Err(format!(
            "`if:` mixes classifier jobs {:?}",
            cs.iter().collect::<Vec<_>>()
        ));
    }
    let outs: Vec<String> = atoms.iter().flatten().map(|a| a[2].to_string()).collect();
    if outs.is_empty() {
        return Err("`if:` reads no classifier output".into());
    }
    Ok((cs.into_iter().next().unwrap(), outs))
}

struct Skip {
    problem: Option<Problem>,
    /// (classifier job, output) pairs this job reads.
    used: Vec<(String, String)>,
}

/// Every way a required job can be skipped other than through the classifier:
///  - job `if:`: absent, or exactly the gated form, wired to the classifier
///    step and reading only the outputs `[workflows.job_outputs]` gives it;
///  - `needs:` only classifier jobs and other required jobs; a need that has
///    an `if:` of its own can be skipped, which skips this job through the
///    implicit success(), unless this job carries the classifier form;
///  - no step `if:` reading `needs.*` or `steps.*.outputs` (gale#319);
///  - no `continue-on-error` on the job; on a step only if a later step reads
///    its outcome/conclusion or it is on `[workflows.coe_exempt]`;
///  - a matrix this check can prove non-empty;
///  - the classifier step: exact `run:`/`env:`, no `if:`/`continue-on-error`
///    on step or job, no EVENT_NAME/BASE_SHA/GATE_PROBE_FILES in job- or
///    workflow-level env.
fn job_skip_state(cfg: &Config, wf: &Wf, jid: &str, required_jids: &BTreeSet<String>) -> Skip {
    let skippable = |code: &'static str, msg: String, used: Vec<(String, String)>| Skip {
        problem: Some((code, msg)),
        used,
    };
    let jobs = entries(get(&wf.doc, "jobs"));
    let job = jobs
        .iter()
        .find(|(j, _)| j == jid)
        .map(|(_, y)| *y)
        .unwrap_or(&Yaml::Null);
    let name = job_name(job, jid);
    let cls = classifier_jobs(&wf.doc);
    let needs: Vec<String> = match get(job, "needs") {
        Some(Yaml::String(s)) => vec![s.clone()],
        Some(Yaml::Array(a)) => a.iter().map(py_str).collect(),
        _ => vec![],
    };
    let cond = get_nn(job, "if");
    let mut used = vec![];

    if has(job, "continue-on-error") {
        return skippable(
            "continue-on-error-job",
            "`continue-on-error` on a required job: it reports success whatever fails".into(),
            used,
        );
    }
    let steps = list(get(job, "steps"));
    let step_if = Regex::new(r"\bneeds\.|\bsteps\.[A-Za-z0-9_-]+\.outputs\b").unwrap();
    for (i, st) in steps.iter().enumerate() {
        let label = get_nn(st, "name")
            .map(py_str)
            .unwrap_or_else(|| i.to_string());
        let sif = get(st, "if").map(py_str).unwrap_or_default();
        if step_if.is_match(&sif) {
            return skippable(
                "step-if-output",
                format!(
                    "step {label} has `if: {sif}`: a step-level skip reports the job green having checked nothing"
                ),
                used,
            );
        }
        if has(st, "continue-on-error") {
            let observed = get_nn(st, "id").is_some_and(|sid| {
                let mut later = String::new();
                steps[i + 1..].iter().for_each(|s| flatten(s, &mut later));
                Regex::new(&format!(
                    r"steps\.{}\.(outcome|conclusion)",
                    regex::escape(&py_str(sid))
                ))
                .unwrap()
                .is_match(&later)
            });
            let exempt = cfg.workflows.coe_exempt.contains_key(&format!(
                "{}:{jid}:{}",
                wf.file,
                get_nn(st, "name").map(py_str).unwrap_or_default()
            ));
            if !observed && !exempt {
                return skippable(
                    "continue-on-error-step",
                    format!(
                        "step {label} has `continue-on-error` and no later step reads its outcome (nor is it on [workflows.coe_exempt])"
                    ),
                    used,
                );
            }
        }
    }

    if let Some(cond) = cond {
        let Yaml::String(cond) = cond else {
            return skippable(
                "if-form",
                format!("`if: {}` is not the classifier form", py_str(cond)),
                used,
            );
        };
        let (c, outs) = match parse_gate_if(cond) {
            Ok(x) => x,
            Err(why) => return skippable("if-form", why, used),
        };
        let Some((_, wired)) = cls.get(&c) else {
            return skippable(
                "classifier-not-run",
                format!("`if:` reads needs.{c}, which does not run `sieve classify`"),
                used,
            );
        };
        if !needs.contains(&c) {
            return skippable(
                "classifier-not-needed",
                format!("`if:` reads needs.{c} but the job does not need it"),
                used,
            );
        }
        let probs = classifier_wiring(&wf.doc, &c, &cfg.path);
        if !probs.is_empty() {
            return skippable("classifier-wiring", probs.join("; "), used);
        }
        for o in &outs {
            if !wired.contains(o) {
                return skippable(
                    "output-unwired",
                    format!("`if:` reads output {o:?}, which job {c} does not wire to the classifier step"),
                    used,
                );
            }
        }
        let Some(allowed) = cfg.workflows.job_outputs.get(&name) else {
            return skippable(
                "job-not-mapped",
                "gated by the classifier but not on the hand-maintained [workflows.job_outputs] map"
                    .into(),
                used,
            );
        };
        let stray: BTreeSet<&String> = outs.iter().filter(|o| !allowed.contains(o)).collect();
        if !stray.is_empty() {
            return skippable(
                "output-not-allowed",
                format!("reads {stray:?}, another job's classifier output ([workflows.job_outputs] allows {allowed:?})"),
                used,
            );
        }
        used = outs.iter().map(|o| (c.clone(), o.clone())).collect();
    }

    for n in &needs {
        if cls.contains_key(n) {
            if cond.is_none() {
                return skippable(
                    "needs-classifier-without-if",
                    format!("needs classifier {n} without the classifier `if:`: a failed classifier skips it"),
                    used,
                );
            }
            continue;
        }
        if !required_jids.contains(n) {
            return skippable(
                "needs-not-required",
                format!("needs {n}, which is not itself required: if {n} fails or skips, this job is skipped and reports success"),
                used,
            );
        }
        let need_job = jobs.iter().find(|(j, _)| j == n).map(|(_, y)| *y);
        if need_job.is_some_and(|nj| get_nn(nj, "if").is_some()) && cond.is_none() {
            return skippable(
                "needs-gated-job",
                format!("needs {n}, which has its own `if:`: when {n} is skipped this job is skipped too (implicit success()), unless it carries the classifier form"),
                used,
            );
        }
    }

    if let Some(m) = get(job, "strategy").and_then(|s| get_nn(s, "matrix")) {
        let bad = match m {
            Yaml::Hash(h) => {
                get(m, "exclude").is_some_and(truthy)
                    || h.iter().any(|(k, v)| {
                        !key_is(k, "include") && !matches!(v, Yaml::Array(a) if !a.is_empty())
                    })
                    || (h.len() == 1 && get(m, "include").is_some_and(|i| !truthy(i)))
            }
            _ => true,
        };
        if bad {
            return skippable(
                "matrix-unprovable",
                "a matrix this check cannot prove non-empty (expression, `exclude:`, or empty axis)"
                    .into(),
                used,
            );
        }
    }
    Skip {
        problem: None,
        used,
    }
}

// ------------------------------------------------------------------- report

#[derive(Default)]
struct Report {
    n: usize,
    failures: Vec<serde_json::Value>,
    refused: Vec<serde_json::Value>,
    ok: Vec<(String, String)>,
    text: Vec<String>,
}

impl Report {
    #[allow(clippy::too_many_arguments)]
    fn fail(&mut self, gate: &str, ctx: &str, wf: &str, job: &str, code: &str, msg: String) {
        self.text.push(format!(
            "  FAIL [{gate}] {code:<22} {ctx}   [{wf}] -- {msg}"
        ));
        self.failures.push(json!({
            "gate": gate, "context": ctx, "workflow": wf, "job": job,
            "code": code, "message": msg,
        }));
    }
    fn refuse(&mut self, input: &str, msg: impl Into<String>) {
        self.refused
            .push(json!({"input": input, "message": msg.into()}));
    }
}

pub fn cmd_check_workflows(
    workflows: Option<&str>,
    required: Option<&str>,
    format: Option<&str>,
    root: PathBuf,
    cfg: &Config,
) -> ExitCode {
    let format = format.unwrap_or("text");
    if !["json", "text"].contains(&format) {
        eprintln!("sieve: unknown --format `{format}`");
        return ExitCode::from(2);
    }
    let at = |p: &str| -> PathBuf {
        if Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            root.join(p)
        }
    };
    let wfdir = at(workflows.unwrap_or(DEFAULT_WORKFLOWS));
    let req_rel = required
        .or(cfg.workflows.required_contexts.as_deref())
        .unwrap_or(DEFAULT_REQUIRED);
    let reqfile = at(req_rel);
    let mut r = Report::default();

    let contexts: Vec<String> = match std::fs::read_to_string(&reqfile) {
        Ok(t) => t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(e) => {
            r.refuse(
                &reqfile.to_string_lossy(),
                format!("required contexts: {e}"),
            );
            vec![]
        }
    };
    let mut files: Vec<(String, PathBuf)> = vec![];
    match std::fs::read_dir(&wfdir) {
        Ok(rd) => {
            let name_re = Regex::new(r"^.*\.y.*ml$").unwrap();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if name_re.is_match(&n) && e.path().is_file() {
                    files.push((n, e.path()));
                }
            }
            files.sort();
        }
        Err(e) => r.refuse(
            &wfdir.to_string_lossy(),
            format!("workflows directory: {e}"),
        ),
    }
    if !r.refused.is_empty() {
        return finish(r, format, &contexts, &wfdir);
    }

    let mut wfs: Vec<Wf> = vec![];
    for (name, path) in &files {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| YamlLoader::load_from_str(&t).map_err(|e| e.to_string()));
        r.n += 1;
        match parsed {
            Err(e) => {
                // The reference skips an unparseable file; a file whose jobs
                // cannot be read is a gate that cannot run, so it fails here.
                r.fail(
                    "a",
                    "",
                    name,
                    "",
                    "unparseable",
                    format!("unparseable ({e})"),
                );
            }
            Ok(docs) => {
                if let Some(doc @ Yaml::Hash(_)) = docs.into_iter().next() {
                    let state = trigger_state(&doc);
                    wfs.push(Wf {
                        file: name.clone(),
                        doc,
                        state,
                    });
                }
            }
        }
    }
    // job display name -> hits
    let mut idx: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
    for (wi, wf) in wfs.iter().enumerate() {
        for (jid, job) in entries(get(&wf.doc, "jobs")) {
            idx.entry(job_name(job, &jid))
                .or_default()
                .push(Hit { wf: wi, jid });
        }
    }
    let matrix_suffix = Regex::new(r"\s*\(.*\)\s*$").unwrap();
    let lookup = |ctx: &str| -> Vec<&Hit> {
        idx.get(ctx)
            .or_else(|| idx.get(matrix_suffix.replace(ctx, "").as_ref()))
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    };

    // Job ids, per workflow file, that host a required context: a need on one
    // of these is visible, because if it fails its own required check is red.
    let mut required_jids: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for ctx in &contexts {
        for h in lookup(ctx) {
            required_jids.entry(h.wf).or_default().insert(h.jid.clone());
        }
    }
    let mut classifier_outputs: BTreeSet<String> = BTreeSet::new();
    let mut classifier_where: BTreeSet<String> = BTreeSet::new();
    let none = BTreeSet::new();
    for ctx in &contexts {
        let hits = lookup(ctx);
        r.n += 1;
        if hits.is_empty() {
            r.fail(
                "a",
                ctx,
                "",
                ctx,
                "unmapped",
                "no job in any workflow produces this name".into(),
            );
            continue;
        }
        for h in hits {
            let wf = &wfs[h.wf];
            let mut problem = wf.state.clone().map(|(c, d)| ("a", c, d));
            if problem.is_none() {
                let s = job_skip_state(cfg, wf, &h.jid, required_jids.get(&h.wf).unwrap_or(&none));
                for (c, o) in s.used {
                    classifier_outputs.insert(o);
                    classifier_where.insert(format!("{}:{c}", wf.file));
                }
                problem = s.problem.map(|(c, d)| ("b", c, d));
            }
            match problem {
                None => {
                    r.text
                        .push(format!("    ok             {ctx}   [{}]", wf.file));
                    r.ok.push((ctx.clone(), wf.file.clone()));
                }
                Some((gate, code, detail)) => r.fail(gate, ctx, &wf.file, &h.jid, code, detail),
            }
        }
    }

    // (c) the classifier it reads fails OPEN
    if !classifier_outputs.is_empty() {
        let where_ = classifier_where
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if !root.join(".git").exists() {
            r.refuse("c", "no .git: the fail-open spot checks need git");
        } else {
            let cases = crate::check::fail_open_cases(&root, cfg);
            let n = cases.len();
            let mut held = true;
            for (ok, msg) in cases {
                r.n += 1;
                if !ok {
                    held = false;
                    r.fail(
                        "c",
                        "",
                        "",
                        "(classifier)",
                        "fails-closed",
                        format!("{msg} [{where_}]"),
                    );
                }
            }
            if held {
                r.text.push(format!(
                    "    fails open     sieve classify: {n} scenario(s) x {} class(es), every output of {} read is 'true'  [{where_}]",
                    cfg.classes.len(),
                    classifier_outputs.len()
                ));
            }
        }
    }
    finish(r, format, &contexts, &wfdir)
}

fn finish(r: Report, format: &str, contexts: &[String], wfdir: &Path) -> ExitCode {
    if format == "json" {
        let out = json!({
            "schema": crate::WORKFLOWS_SCHEMA,
            "pass": r.failures.is_empty() && r.refused.is_empty(),
            "checks": r.n,
            "failures": r.failures,
            "refused": r.refused,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        println!(
            "  {} required context(s) against {}\n",
            contexts.len(),
            wfdir.display()
        );
        for l in &r.text {
            println!("{l}");
        }
        for f in &r.refused {
            println!(
                "  REFUSED [{}] {}",
                f["input"].as_str().unwrap_or(""),
                f["message"].as_str().unwrap_or("")
            );
        }
        println!(
            "\n    UNGUARDED: that this file still matches the real branch protection was NOT\n    checked (reading protection needs admin rights the default GITHUB_TOKEN lacks).\n    The [workflows.job_outputs] map is HAND-MAINTAINED: this check proves a job reads\n    only its own outputs, not that the map is right (the false-skip controls test that).\n"
        );
        if !r.failures.is_empty() {
            println!(
                "  FAIL: {} problem(s): a required context may not be produced, or may be skipped by something other than the change classifier",
                r.failures.len()
            );
        } else if !r.refused.is_empty() {
            println!("  REFUSED: {} input(s) missing", r.refused.len());
        } else {
            println!(
                "  PASS: {} required context(s) map to jobs that report on every PR and in the merge queue,\n        and can be skipped only through the classifier wiring this check verifies",
                contexts.len()
            );
        }
    }
    if !r.failures.is_empty() {
        ExitCode::from(1)
    } else if !r.refused.is_empty() {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}
