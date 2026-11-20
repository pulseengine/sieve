//! The language-neutral classifier corpus (tests/corpus/, vendored from gale
//! 4050000f) run against the shipped binary.
//!
//! `selected` is exact in both directions: a class selected beyond it wastes
//! runners, a class missing from it is a false skip. Every case runs in json
//! and github-output format; every paths-mode case also runs in a tree with
//! no .git (plant's mode). One test reports every case and fails on any.

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");
/// sieve runs with an empty PATH: it must start no `git` (or any other)
/// process (REQ-024). A subprocess would fail and the git-mode cases would
/// select the wrong set.
const NO_PATH: &str = "/nonexistent-sieve-starts-no-subprocess";

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn load(p: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

fn toml_list(v: &[String]) -> String {
    format!(
        "[{}]",
        v.iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// spec.json (run-all, inert) plus a case's gates, as a sieve.toml.
fn config(spec: &Value, gates: &Value) -> String {
    let mut t = String::from("schema = \"sieve.config.v1\"\n\n[run_all]\n");
    for k in ["files", "prefixes", "suffixes", "basenames"] {
        t += &format!("{k} = {}\n", toml_list(&strs(&spec["run_all"][k])));
    }
    t += &format!(
        "\n[inert]\nfiles = {}\nprefixes = {}\n",
        toml_list(&strs(&spec["inert"]["files"])),
        toml_list(&strs(&spec["inert"]["prefixes"]))
    );
    // The reference excludes itself and the gate-probe contract from the docs rule.
    t += "\n[docs]\nnamer_exclude = [\"tools/ci/classify-changes.py\", \".github/gate-probe.json\"]\n";
    t += "\n[zephyr]\ncmake_root_vars = [\"GALE_DIR\"]\n";
    for (name, g) in gates.as_object().unwrap() {
        t += &format!("\n[class.{name}]\n");
        for k in ["cargo", "cargo_glob", "zephyr", "hand", "bazel_build"] {
            if g.get(k).is_some() {
                t += &format!("{k} = {}\n", toml_list(&strs(&g[k])));
            }
        }
    }
    t
}

fn git(dir: &Path, args: &[&str]) -> String {
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
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write_tree(dir: &Path, fixture: &Value, case: &Value) {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    let mut files: std::collections::BTreeMap<String, String> = fixture["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    if let Some(set) = case["tree"]["set"].as_object() {
        for (k, v) in set {
            files.insert(k.clone(), v.as_str().unwrap().to_string());
        }
    }
    for d in strs(&case["tree"]["delete"]) {
        files.remove(&d);
    }
    if let Some(cf) = case["check"]["files"].as_object() {
        for (k, v) in cf {
            files.insert(k.clone(), v.as_str().unwrap().to_string());
        }
    }
    for (p, body) in files {
        let fp = dir.join(&p);
        fs::create_dir_all(fp.parent().unwrap()).unwrap();
        fs::write(fp, body).unwrap();
    }
}

struct Run {
    selected: BTreeSet<String>,
    warning: bool,
    detail: String,
}

/// The command that runs sieve for one invocation. Natively it is the built
/// binary. With SIEVE_WASMTIME and SIEVE_WASM set it is the WASI component
/// under that runtime (REQ-031): the tree is preopened as `.`, the scratch
/// root at its own absolute path, and only the listed variables reach the
/// guest. Native and component runs must produce the same bytes.
fn component_mode() -> bool {
    std::env::var_os("SIEVE_WASMTIME").is_some() && std::env::var_os("SIEVE_WASM").is_some()
}

fn sieve_cmd(dir: &Path, env: &[(&str, String)]) -> Command {
    match (std::env::var("SIEVE_WASMTIME"), std::env::var("SIEVE_WASM")) {
        (Ok(rt), Ok(wasm)) => {
            let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("corpus");
            let mut c = Command::new(rt);
            // wasmtime itself keeps the host environment (HOME for its cache
            // config); the GUEST sees only the --env variables below, since
            // wasmtime passes no host variable to it by default
            c.env_remove("GATE_PROBE_FILES")
                .env_remove("SIEVE_FAULT")
                .env_remove("GITHUB_OUTPUT")
                .arg("run")
                // newer Rust std (CI's stable) imports wasi:cli/exit's
                // exit-with-code, which wasmtime gates as unstable
                .args(["-S", "cli-exit-with-code=y"])
                .arg("--dir")
                .arg(format!("{}::.", dir.display()))
                .arg("--dir")
                .arg(format!("{0}::{0}", scratch.display()));
            for (k, v) in env {
                c.arg("--env").arg(format!("{k}={v}"));
            }
            c.arg(wasm).current_dir(dir);
            c
        }
        _ => {
            let mut c = Command::new(BIN);
            c.env("PATH", NO_PATH)
                .env_remove("GATE_PROBE_FILES")
                .env_remove("SIEVE_FAULT")
                .env_remove("GITHUB_OUTPUT")
                .current_dir(dir);
            for (k, v) in env {
                c.env(k, v);
            }
            c
        }
    }
}

fn run_classify(
    dir: &Path,
    cfg: &Path,
    args: &[String],
    env: &[(&str, String)],
    all: &BTreeSet<String>,
) -> Result<Run, String> {
    // json
    let mut cmd = sieve_cmd(dir, env);
    cmd.arg("classify")
        .arg("--config")
        .arg(cfg)
        .args(["--format", "json"])
        .args(args);
    let o = cmd.output().unwrap();
    if component_mode() {
        // the component must print the native binary's bytes exactly
        let mut native = Command::new(BIN);
        native
            .env("PATH", NO_PATH)
            .env_remove("GATE_PROBE_FILES")
            .env_remove("SIEVE_FAULT")
            .env_remove("GITHUB_OUTPUT")
            .current_dir(dir)
            .arg("classify")
            .arg("--config")
            .arg(cfg)
            .args(["--format", "json"])
            .args(args);
        for (k, v) in env {
            native.env(k, v);
        }
        let n = native.output().unwrap();
        if n.stdout != o.stdout || n.status.code() != o.status.code() {
            return Err(format!(
                "component differs from native: exit {:?} vs {:?}\n--- native\n{}\n--- component\n{}\n--- component stderr\n{}",
                n.status.code(),
                o.status.code(),
                String::from_utf8_lossy(&n.stdout),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ));
        }
    }
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    if o.status.code() != Some(0) {
        return Err(format!("json run exited {:?}: {stderr}", o.status.code()));
    }
    let v: Value = serde_json::from_slice(&o.stdout).map_err(|e| format!("json: {e}"))?;
    if v["schema"] != "sieve.classify.v1" {
        return Err(format!("schema {}", v["schema"]));
    }
    let selected: BTreeSet<String> = v["classes"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, b)| b.as_bool() == Some(true))
        .map(|(k, _)| k.clone())
        .collect();
    let keys: BTreeSet<String> = v["classes"].as_object().unwrap().keys().cloned().collect();
    if &keys != all {
        return Err(format!("classes {keys:?} != configured {all:?}"));
    }
    if v["run_all"] == true && &selected != all {
        return Err("run_all true but not every class selected".into());
    }
    let reason = v["reason"].as_str().unwrap_or("").to_string();
    let warning = stderr.contains("::warning");
    if warning != reason.starts_with("internal:") {
        return Err(format!(
            "warning {warning} disagrees with reason {reason:?}"
        ));
    }

    // github-output: the same verdict through $GITHUB_OUTPUT
    let out = dir.parent().unwrap().join(format!(
        "{}.github_output",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_file(&out);
    let mut env2 = env.to_vec();
    env2.push(("GITHUB_OUTPUT", out.to_string_lossy().into_owned()));
    let mut cmd = sieve_cmd(dir, &env2);
    cmd.arg("classify")
        .arg("--config")
        .arg(cfg)
        .args(["--format", "github-output"])
        .args(args);
    let o2 = cmd.output().unwrap();
    if o2.status.code() != Some(0) {
        return Err(format!("github-output run exited {:?}", o2.status.code()));
    }
    let written = fs::read_to_string(&out).map_err(|e| format!("GITHUB_OUTPUT: {e}"))?;
    let gh: BTreeSet<String> = written
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, v)| *k != "run_all" && *v == "true")
        .map(|(k, _)| k.to_string())
        .collect();
    if gh != selected {
        return Err(format!("github-output selects {gh:?}, json {selected:?}"));
    }
    if warning != String::from_utf8_lossy(&o2.stdout).contains("::warning") {
        return Err("github-output and json disagree on the warning".into());
    }
    Ok(Run {
        selected,
        warning,
        detail: format!("reason={reason:?} unknown={}", v["unknown_paths"]),
    })
}

fn check_case(
    name: &str,
    case: &Value,
    spec: &Value,
    fixture: &Value,
    scratch: &Path,
) -> Result<(), String> {
    let gates = case.get("gates").unwrap_or(&spec["gates"]);
    let all: BTreeSet<String> = gates.as_object().unwrap().keys().cloned().collect();
    let cfg = scratch.join(format!("{name}.sieve.toml"));
    fs::write(&cfg, config(spec, gates)).unwrap();
    let dir = scratch.join(name);
    write_tree(&dir, fixture, case);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let first = git(&dir, &["rev-parse", "HEAD"]);

    let input = &case["input"];
    let mode = input["mode"].as_str().unwrap();
    let mut args: Vec<String> = vec![];
    let mut env: Vec<(&str, String)> = vec![];
    if input["inject"]["no_toml_parser"] == true {
        env.push(("SIEVE_FAULT", "toml".into()));
    }
    let paths_file = scratch.join(format!("{name}.paths"));
    match mode {
        "paths" => {
            fs::write(&paths_file, strs(&input["paths"]).join("\n")).unwrap();
            args.extend([
                "--paths-file".into(),
                paths_file.to_string_lossy().into_owned(),
            ]);
            if let Some(ev) = input["event"].as_str() {
                args.extend(["--event".into(), ev.into()]);
            }
        }
        "probe" => {
            fs::write(&paths_file, strs(&input["paths"]).join("\n") + "\n").unwrap();
            env.push((
                "GATE_PROBE_FILES",
                paths_file.to_string_lossy().into_owned(),
            ));
        }
        "git" => {
            for ch in input["change"].as_array().into_iter().flatten() {
                match ch["op"].as_str().unwrap() {
                    "append" => {
                        let p = dir.join(ch["path"].as_str().unwrap());
                        let mut s = fs::read_to_string(&p).unwrap_or_default();
                        s += ch["text"].as_str().unwrap();
                        fs::write(&p, s).unwrap();
                    }
                    "delete" => fs::remove_file(dir.join(ch["path"].as_str().unwrap())).unwrap(),
                    "rename" => {
                        let to = dir.join(ch["to"].as_str().unwrap());
                        fs::create_dir_all(to.parent().unwrap()).unwrap();
                        fs::rename(dir.join(ch["from"].as_str().unwrap()), to).unwrap();
                    }
                    op => return Err(format!("unknown change op {op}")),
                }
            }
            git(&dir, &["add", "-A"]);
            git(&dir, &["commit", "-q", "--allow-empty", "-m", "change"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            match input["base"].as_str().unwrap() {
                "parent" => args.extend(["--base".into(), first.clone()]),
                "empty" => args.extend(["--base".into(), head]),
                "invalid" => args.extend(["--base".into(), "0".repeat(40)]),
                "none" => {}
                b => return Err(format!("unknown base {b}")),
            }
            if let Some(ev) = input["event"].as_str().filter(|e| !e.is_empty()) {
                args.extend(["--event".into(), ev.into()]);
            }
        }
        m => return Err(format!("unknown mode {m}")),
    }

    let expect = &case["expect"];
    let want: BTreeSet<String> = if expect["selected"] == "all" {
        all.clone()
    } else {
        strs(&expect["selected"]).into_iter().collect()
    };
    let got = run_classify(&dir, &cfg, &args, &env, &all)?;
    if got.selected != want {
        let missing: Vec<_> = want.difference(&got.selected).collect();
        let extra: Vec<_> = got.selected.difference(&want).collect();
        return Err(format!(
            "{}selects {:?}, want {:?} (missing {missing:?}, extra {extra:?}); {}",
            if missing.is_empty() {
                ""
            } else {
                "FALSE SKIP: "
            },
            got.selected,
            want,
            got.detail
        ));
    }
    if expect["warning"] == true && !got.warning {
        return Err("expected a ::warning::".into());
    }

    // The same paths case with no .git at all.
    if mode == "paths" {
        fs::remove_dir_all(dir.join(".git")).unwrap();
        let nogit =
            run_classify(&dir, &cfg, &args, &env, &all).map_err(|e| format!("no-.git run: {e}"))?;
        if nogit.selected != want {
            return Err(format!(
                "no-.git run selects {:?}, want {want:?}",
                nogit.selected
            ));
        }
    }

    if let Some(kind) = case["check"]["kind"].as_str() {
        let mut cmd = Command::new(BIN);
        cmd.env("PATH", NO_PATH);
        cmd.arg("check-graph")
            .arg("--config")
            .arg(&cfg)
            .args(["--format", "json", "--control", kind])
            .current_dir(&dir);
        if kind == "depinfo" {
            let gate = case["check"]["gate"].as_str().unwrap();
            for f in case["check"]["files"].as_object().unwrap().keys() {
                cmd.args(["--depinfo", &format!("{gate}={f}")]);
            }
        }
        let o = cmd.output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout).map_err(|e| {
            format!(
                "check-graph json: {e}: {}",
                String::from_utf8_lossy(&o.stderr)
            )
        })?;
        let want_pass = expect["check"] == "pass";
        if (v["pass"] == true) != want_pass {
            return Err(format!(
                "check-graph pass={} want {}: {}",
                v["pass"], expect["check"], v["failures"]
            ));
        }
        let code = o.status.code();
        if code != Some(if want_pass { 0 } else { 1 }) {
            return Err(format!("check-graph exit {code:?}"));
        }
        if let Some(err) = expect["error"].as_str() {
            let codes: Vec<String> = v["failures"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|f| f["code"].as_str().map(str::to_string))
                .collect();
            if !codes.iter().any(|c| c == err) {
                return Err(format!("check-graph error codes {codes:?}, want {err}"));
            }
        }
    }
    Ok(())
}

// rivet: verifies REQ-024
// rivet: verifies REQ-001
// rivet: verifies REQ-002
// rivet: verifies REQ-003
// rivet: verifies REQ-004
// rivet: verifies REQ-005
// rivet: verifies REQ-006
// rivet: verifies REQ-007
// rivet: verifies REQ-008
// rivet: verifies REQ-009
// rivet: verifies REQ-010
// rivet: verifies REQ-012
// rivet: verifies FEAT-001
#[test]
fn corpus_every_case_selects_exactly_its_expected_set() {
    let spec = load(&corpus().join("spec.json"));
    let fixture = load(&corpus().join("fixture-default.json"));
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("corpus");
    fs::create_dir_all(&scratch).unwrap();
    let mut cases: Vec<PathBuf> = fs::read_dir(corpus().join("cases"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    cases.sort();
    let only = std::env::var("SIEVE_CASE").ok();
    let mut failures = vec![];
    let mut n = 0;
    let mut skipped = 0;
    for p in &cases {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        let case = load(p);
        if component_mode() && case["input"]["mode"] != "paths" {
            // gix reads objects through mmap, which WASI lacks: git mode
            // fails open in the component (REQ-031 covers paths mode)
            println!("skip {name}: git mode, not run in component mode");
            skipped += 1;
            continue;
        }
        n += 1;
        match check_case(&name, &case, &spec, &fixture, &scratch) {
            Ok(()) => println!("ok   {name}"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                failures.push(name);
            }
        }
    }
    println!(
        "\n{}/{} corpus case(s) pass ({skipped} git-mode case(s) not run)",
        n - failures.len(),
        n
    );
    assert!(
        only.is_some() || n + skipped == 74,
        "expected 74 corpus cases, found {}",
        n + skipped
    );
    assert!(
        only.is_some() || !component_mode() || n == 55,
        "component mode must run all 55 paths-mode cases, ran {n}"
    );
    assert!(failures.is_empty(), "failing cases: {failures:?}");
}

// rivet: verifies REQ-031
/// In the WASI component, git mode fails OPEN and says why on stdout, so a
/// consumer (plant's controls) sees the reason instead of a silent run-all.
/// Runs only in component mode; natively there is nothing to check.
#[test]
fn component_git_mode_fails_open_with_its_reason() {
    if !component_mode() {
        return;
    }
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("corpus")
        .join("component-git");
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("a")).unwrap();
    fs::write(d.join("a/x"), "1").unwrap();
    fs::write(d.join("sieve.toml"), "[class.a]\nhand = [\"a/\"]\n").unwrap();
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "root"]);
    fs::write(d.join("a/x"), "2").unwrap();
    git(&d, &["commit", "-q", "-am", "change"]);
    let o = sieve_cmd(&d, &[])
        .args(["classify", "--config", "sieve.toml", "--format", "json"])
        .args([
            "--event",
            "pull_request",
            "--base",
            "HEAD~1",
            "--head",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["run_all"], true, "{v}");
    assert!(
        v["reason"]
            .as_str()
            .is_some_and(|r| r.contains("unsupported in the WASI component")),
        "{v}"
    );
}
