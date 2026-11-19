//! `sieve observe`: the observed-miss counter (REQ-019). Given sieve's
//! verdict for a commit and the job conclusions of a run where every job ran
//! (on main, or every Nth merge-queue run), report each job that failed while
//! sieve would have skipped it: its classes, per the HAND-MAINTAINED
//! `[workflows.job_outputs]` map, were all false and run_all was false. A
//! field oracle beside plant's planted ones. Exit 0 no miss, 1 a miss,
//! 2 usage, 3 refused (unreadable input, or no gated job to judge).

use crate::config::Config;
use serde_json::{json, Value};
use std::process::ExitCode;

/// A job conclusion that means the job found a problem.
fn failed(conclusion: &str) -> bool {
    matches!(conclusion, "failure" | "timed_out")
}

/// The job_outputs entry for a job name, allowing a matrix suffix
/// ("Test (ubuntu-24.04)" matches "Test").
fn classes_of<'a>(cfg: &'a Config, name: &str) -> Option<&'a Vec<String>> {
    let map = &cfg.workflows.job_outputs;
    map.get(name).or_else(|| {
        let base = name.split_once(" (").map(|(b, _)| b)?;
        map.get(base)
    })
}

pub fn cmd(
    verdict: Option<&str>,
    results: Option<&str>,
    format: Option<&str>,
    cfg: &Config,
) -> ExitCode {
    let format = format.unwrap_or("text");
    if !["json", "text"].contains(&format) {
        eprintln!("sieve: unknown --format `{format}`");
        return ExitCode::from(2);
    }
    let (Some(vp), Some(rp)) = (verdict, results) else {
        eprintln!("sieve: observe needs --verdict <classify.json> and --results <jobs.json>");
        return ExitCode::from(2);
    };
    let load = |p: &str| -> Result<Value, String> {
        let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        serde_json::from_str(&t).map_err(|e| format!("{p}: {e}"))
    };
    let (v, r) = match (load(vp), load(rp)) {
        (Ok(v), Ok(r)) => (v, r),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("sieve observe: refused: {e}");
            return ExitCode::from(3);
        }
    };
    if v["schema"] != crate::CLASSIFY_SCHEMA {
        eprintln!(
            "sieve observe: refused: {vp} is not a {} verdict",
            crate::CLASSIFY_SCHEMA
        );
        return ExitCode::from(3);
    }
    // Either [{name, conclusion}] or the API's {"jobs": [...]}.
    let jobs: Vec<&Value> = match (&r, r.get("jobs")) {
        (Value::Array(a), _) => a.iter().collect(),
        (_, Some(Value::Array(a))) => a.iter().collect(),
        _ => {
            eprintln!("sieve observe: refused: {rp} holds no job list");
            return ExitCode::from(3);
        }
    };
    let run_all = v["run_all"] == true;
    let selected = |c: &str| v["classes"].get(c).and_then(Value::as_bool).unwrap_or(true);
    let (mut gated, mut gated_failed, mut failed_total) = (0usize, 0usize, 0usize);
    let mut misses = Vec::new();
    for j in &jobs {
        let name = j["name"].as_str().unwrap_or("");
        let concl = j["conclusion"].as_str().unwrap_or("");
        let is_failed = failed(concl);
        failed_total += usize::from(is_failed);
        let Some(classes) = classes_of(cfg, name) else {
            continue;
        }; // ungated: it ran anyway
        gated += 1;
        if !is_failed {
            continue;
        }
        gated_failed += 1;
        let would_skip = !run_all && classes.iter().all(|c| !selected(c));
        if would_skip {
            misses.push(json!({"job": name, "conclusion": concl, "classes": classes}));
        }
    }
    let refused = gated == 0;
    if format == "json" {
        let out = json!({
            "schema": crate::OBSERVE_SCHEMA,
            "verdict_run_all": run_all,
            "jobs": jobs.len(),
            "gated_jobs": gated,
            "failed_jobs": failed_total,
            "gated_failed_jobs": gated_failed,
            "misses": misses,
            "refused": refused,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for m in &misses {
            println!(
                "  MISS {} ({}): sieve selected none of {}",
                m["job"].as_str().unwrap_or(""),
                m["conclusion"].as_str().unwrap_or(""),
                m["classes"]
            );
        }
        println!(
            "  {} job(s), {} gated, {} failed ({} gated); {} observed miss(es){}",
            jobs.len(),
            gated,
            failed_total,
            gated_failed,
            misses.len(),
            if refused {
                "; refused: no job matches [workflows.job_outputs]"
            } else {
                ""
            }
        );
    }
    if refused {
        ExitCode::from(3)
    } else if !misses.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
