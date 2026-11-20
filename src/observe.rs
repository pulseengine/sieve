//! `sieve observe`: the observed-miss counter (REQ-019). Given sieve's
//! verdict for a commit and the job conclusions of a run where every job ran
//! (on main, or every Nth merge-queue run), report each job that failed while
//! sieve would have skipped it: its classes, per the HAND-MAINTAINED
//! `[workflows.job_outputs]` map, were all false and run_all was false. A
//! field oracle beside plant's planted ones. Exit 0 no miss, 1 a miss,
//! 2 usage, 3 refused (unreadable input, or no gated job to judge).
//!
//! REQ-029: misses are reported as m of N, N being the jobs sieve would have
//! skipped that concluded (success, failure, timed_out) in the full run; a
//! skipped job that was cancelled or itself skipped is listed, not counted.
//! With a consumer cost table the saving is an ESTIMATE by construction:
//! each skipped job's median cost, summed; a job without a cost is listed,
//! never counted as zero. The table also says whether a skipped job still
//! runs before merge (merge queue) or only after it.

use crate::config::Config;
use serde_json::{json, Value};
use std::process::ExitCode;

/// A job conclusion that means the job found a problem.
fn failed(conclusion: &str) -> bool {
    matches!(conclusion, "failure" | "timed_out")
}

/// A conclusion that lets the full run stand in for the skipped job.
fn concluded(conclusion: &str) -> bool {
    matches!(conclusion, "success" | "failure" | "timed_out")
}

/// A map entry for a job name, allowing a matrix suffix
/// ("Test (ubuntu-24.04)" matches "Test").
fn by_job<'a, V>(map: &'a std::collections::BTreeMap<String, V>, name: &str) -> Option<&'a V> {
    map.get(name).or_else(|| {
        let base = name.split_once(" (").map(|(b, _)| b)?;
        map.get(base)
    })
}

fn classes_of<'a>(cfg: &'a Config, name: &str) -> Option<&'a Vec<String>> {
    by_job(&cfg.workflows.job_outputs, name)
}

struct Costs {
    merge_queue: bool,
    jobs: std::collections::BTreeMap<String, f64>,
}

fn load_costs(v: &Value, p: &str) -> Result<Costs, String> {
    if v["schema"] != "sieve.costs.v1" {
        return Err(format!("{p}: schema must be sieve.costs.v1"));
    }
    let merge_queue = v["merge_queue"]
        .as_bool()
        .ok_or(format!("{p}: merge_queue (true or false) is required"))?;
    let mut jobs = std::collections::BTreeMap::new();
    for (k, c) in v["jobs"]
        .as_object()
        .ok_or(format!("{p}: jobs must be an object"))?
    {
        match c.as_f64() {
            Some(m) if m.is_finite() && m >= 0.0 => {
                jobs.insert(k.clone(), m);
            }
            _ => {
                return Err(format!(
                    "{p}: jobs.\"{k}\" must be a non-negative number of minutes"
                ))
            }
        }
    }
    Ok(Costs { merge_queue, jobs })
}

pub fn cmd(
    verdict: Option<&str>,
    results: Option<&str>,
    costs: Option<&str>,
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
    let costs = match costs.map(|p| load(p).and_then(|c| load_costs(&c, p))) {
        None => None,
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => {
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
    let (mut compared, mut uncompared, mut uncosted) = (0usize, Vec::new(), Vec::new());
    let mut saved = 0.0f64;
    for j in &jobs {
        let name = j["name"].as_str().unwrap_or("");
        let concl = j["conclusion"].as_str().unwrap_or("");
        let is_failed = failed(concl);
        failed_total += usize::from(is_failed);
        let Some(classes) = classes_of(cfg, name) else {
            continue;
        }; // ungated: it ran anyway
        gated += 1;
        gated_failed += usize::from(is_failed);
        let would_skip = !run_all && classes.iter().all(|c| !selected(c));
        if !would_skip {
            continue;
        }
        if !concluded(concl) {
            uncompared.push(json!({"job": name, "conclusion": concl}));
            continue;
        }
        compared += 1;
        if let Some(c) = &costs {
            match by_job(&c.jobs, name) {
                Some(m) => saved += m,
                None => uncosted.push(name.to_string()),
            }
        }
        if is_failed {
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
            "compared_skipped_jobs": compared,
            "uncompared_skipped_jobs": uncompared,
            "estimated_saved_runner_minutes": costs.as_ref().map(|_| saved),
            "uncosted_skipped_jobs": uncosted,
            "skipped_jobs_run_before_merge": costs.as_ref().map(|c| c.merge_queue),
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
        if let Some(c) = &costs {
            println!(
                "  estimated saving {saved} runner-min (median cost per skipped job; {} uncosted); skipped jobs run {} merge",
                uncosted.len(),
                if c.merge_queue { "before" } else { "only after" }
            );
        }
        println!(
            "  {} job(s), {} gated, {} failed ({} gated); {} of {} skipped job(s) observed as misses ({} uncompared){}",
            jobs.len(),
            gated,
            failed_total,
            gated_failed,
            misses.len(),
            compared,
            uncompared.len(),
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
