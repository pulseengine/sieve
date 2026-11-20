//! REQ-027: a nested workspace's own Cargo.lock belongs to the classes whose
//! Cargo walk reaches that workspace. The root Cargo.lock, a lockfile no walk
//! reaches, one an adopter lists in [run_all], and one a Bazel file names
//! (a crate_universe lockfile, DD-009) still select everything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

const CFG: &str = "[class.n]\ncargo = [\"nested/ws/m/Cargo.toml\"]\n\
[class.s]\ncargo = [\"standalone/Cargo.toml\"]\n\
[class.a]\ncargo = [\"a/Cargo.toml\"]\n\
[class.h]\nhand = [\"other/\"]\n";

/// root workspace (member a) with the root Cargo.lock; nested/ws, a
/// workspace with member m and its own lock; standalone, a package that
/// declares its own [workspace] and has its own lock; lonely/Cargo.lock,
/// which no manifest owns. `extra` adds or overrides files.
fn tree(name: &str, cfg: &str, extra: &[(&str, &str)]) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("nested_lock")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    let files: Vec<(&str, &str)> = vec![
        ("sieve.toml", cfg),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\"]\nexclude = [\"nested\", \"standalone\"]\n",
        ),
        ("Cargo.lock", "version = 4\n"),
        (
            "a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        ("a/src/lib.rs", "\n"),
        ("nested/ws/Cargo.toml", "[workspace]\nmembers = [\"m\"]\n"),
        ("nested/ws/Cargo.lock", "version = 4\n"),
        (
            "nested/ws/m/Cargo.toml",
            "[package]\nname = \"m\"\nversion = \"0.1.0\"\n",
        ),
        ("nested/ws/m/src/lib.rs", "\n"),
        (
            "standalone/Cargo.toml",
            "[workspace]\n\n[package]\nname = \"s\"\nversion = \"0.1.0\"\n",
        ),
        ("standalone/Cargo.lock", "version = 4\n"),
        ("standalone/src/lib.rs", "\n"),
        ("lonely/Cargo.lock", "version = 4\n"),
        ("other/x", "1"),
    ];
    for (p, t) in files.iter().chain(extra.iter()) {
        let f = d.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, t).unwrap();
    }
    d
}

fn classify(d: &Path, path: &str) -> serde_json::Value {
    let o = Command::new(BIN)
        .args([
            "classify",
            "--config",
            "sieve.toml",
            "--explain",
            "--paths",
            path,
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

fn assert_run_all(v: &serde_json::Value, why: &str) {
    assert_eq!(v["run_all"], true, "{v}");
    let reason = v["reason"].as_str().unwrap_or("");
    assert!(reason.contains(why), "reason {reason:?} lacks {why:?}");
}

// rivet: verifies REQ-027
#[test]
fn a_nested_workspace_lock_selects_the_classes_that_build_it() {
    let d = tree("scoped", CFG, &[]);
    let v = classify(&d, "nested/ws/Cargo.lock");
    assert_eq!(v["run_all"], false, "{v}");
    assert_eq!(selected(&v), ["n"], "{v}");
    assert_eq!(v["explain"]["n"][0]["via"], "cargo-lock", "{v}");

    let v = classify(&d, "standalone/Cargo.lock");
    assert_eq!(v["run_all"], false, "{v}");
    assert_eq!(selected(&v), ["s"], "{v}");
}

// rivet: verifies REQ-027
#[test]
fn the_root_lock_and_an_unowned_lock_select_everything() {
    let d = tree("negatives", CFG, &[]);
    assert_run_all(&classify(&d, "Cargo.lock"), "is CI, toolchain");
    assert_run_all(&classify(&d, "lonely/Cargo.lock"), "no class's Cargo walk");
}

// rivet: verifies REQ-027
#[test]
fn a_lock_bazel_names_selects_everything() {
    for (name, file, text) in [
        (
            "bazel-module-label",
            "MODULE.bazel",
            "crate.from_cargo(cargo_lockfile = \"//standalone:Cargo.lock\")\n",
        ),
        (
            "bazel-module-lock-path",
            "MODULE.bazel.lock",
            "\"FILE:@@//standalone/Cargo.lock abc\"\n",
        ),
        (
            "bazel-build-relative",
            "standalone/BUILD.bazel",
            "exports_files([\"Cargo.lock\"])\n",
        ),
        (
            "bazel-bzl",
            "bzl/deps.bzl",
            "LOCK = \"standalone/Cargo.lock\"\n",
        ),
    ] {
        let d = tree(name, CFG, &[(file, text)]);
        assert_run_all(&classify(&d, "standalone/Cargo.lock"), file);
        // the other nested lock is untouched by that reference
        assert_eq!(
            selected(&classify(&d, "nested/ws/Cargo.lock")),
            ["n"],
            "{name}"
        );
    }
}

// rivet: verifies REQ-027
#[test]
fn an_adopter_run_all_entry_for_the_lock_wins() {
    let cfg = format!("{CFG}[run_all]\nfiles = [\"nested/ws/Cargo.lock\"]\n");
    let d = tree("own-run-all", &cfg, &[]);
    assert_run_all(&classify(&d, "nested/ws/Cargo.lock"), "is CI, toolchain");
}

// rivet: verifies REQ-027
#[test]
fn check_graph_accepts_the_lock_claims() {
    let d = tree("check-graph", CFG, &[]);
    let o = Command::new(BIN)
        .args([
            "check-graph",
            "--config",
            "sieve.toml",
            "--control",
            "graph-inputs",
        ])
        .current_dir(&d)
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

// rivet: verifies REQ-027
#[test]
fn a_lock_selects_every_class_that_claims_inside_its_workspace() {
    // m2 is a second member of nested/ws that no Cargo walk reaches, hand-
    // claimed by h2: the lock pins its build too.
    let cfg = format!("{CFG}[class.h2]\nhand = [\"nested/ws/m2/\"]\n");
    let d = tree(
        "whole-workspace",
        &cfg,
        &[
            (
                "nested/ws/Cargo.toml",
                "[workspace]\nmembers = [\"m\", \"m2\"]\n",
            ),
            (
                "nested/ws/m2/Cargo.toml",
                "[package]\nname = \"m2\"\nversion = \"0.1.0\"\n",
            ),
            ("nested/ws/m2/src/lib.rs", "\n"),
        ],
    );
    let v = classify(&d, "nested/ws/Cargo.lock");
    assert_eq!(v["run_all"], false, "{v}");
    assert_eq!(selected(&v), ["h2", "n"], "{v}");
}

// rivet: verifies REQ-027
#[test]
fn more_bazel_reference_forms_select_everything() {
    for (name, file, text) in [
        (
            "label-from-parent",
            "MODULE.bazel",
            "lock = \"//:standalone/Cargo.lock\"\n",
        ),
        (
            "label-mid",
            "x.bzl",
            "L = Label(\"@@//nested:ws/Cargo.lock\")\n",
        ),
        (
            "glob-in-ancestor-build",
            "BUILD.bazel",
            "filegroup(srcs = glob([\"**/Cargo.lock\"]))\n",
        ),
        (
            "star-file",
            "tools/deps.star",
            "L = \"standalone/Cargo.lock\"\n",
        ),
        (
            "other-bazel-suffix",
            "cargo.MODULE.bazel",
            "L = \"//standalone:Cargo.lock\"\n",
        ),
    ] {
        let d = tree(name, CFG, &[(file, text)]);
        let lock = if name == "label-mid" || name == "glob-in-ancestor-build" {
            "nested/ws/Cargo.lock"
        } else {
            "standalone/Cargo.lock"
        };
        assert_run_all(&classify(&d, lock), file);
    }
    // A BUILD file in a sibling directory that mentions some Cargo.lock does
    // not name this one.
    let d = tree(
        "sibling-build",
        CFG,
        &[("other/BUILD.bazel", "exports_files([\"Cargo.lock\"])\n")],
    );
    assert_eq!(selected(&classify(&d, "standalone/Cargo.lock")), ["s"]);
}

// rivet: verifies REQ-027
#[test]
fn an_adopter_cargo_lock_basename_keeps_every_lock_run_all() {
    let cfg = format!("{CFG}[run_all]\nbasenames = [\"Cargo.lock\"]\n");
    let d = tree("adopter-basename", &cfg, &[]);
    assert_run_all(&classify(&d, "nested/ws/Cargo.lock"), "is CI, toolchain");
}

// rivet: verifies REQ-027
#[test]
fn cold_review_layouts_pick_cargos_lock_or_select_everything() {
    // 1. nested/ws excludes tool, so Cargo builds tool with the ROOT
    //    workspace's lock (the root lists it). tool's walk must not claim
    //    nested/ws/Cargo.lock; nested/ws/Cargo.lock then selects only n.
    let cfg = format!("{CFG}[class.t]\ncargo = [\"nested/ws/tool/Cargo.toml\"]\n");
    let d = tree(
        "exclude",
        &cfg,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"nested/ws/tool\"]\nexclude = [\"standalone\"]\n",
            ),
            (
                "nested/ws/Cargo.toml",
                "[workspace]\nmembers = [\"m\"]\nexclude = [\"tool\"]\n",
            ),
            (
                "nested/ws/tool/Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n",
            ),
            ("nested/ws/tool/src/main.rs", "fn main() {}\n"),
        ],
    );
    let v = classify(&d, "nested/ws/Cargo.lock");
    assert_eq!(v["run_all"], false, "{v}");
    assert_eq!(v["classes"]["n"], true, "{v}");
    // t is selected only for its claims inside nested/ws, not through the lock.
    assert_ne!(v["explain"]["t"][0]["via"], "cargo-lock", "{v}");

    // 2. A member no class claims: the lock pins its build, so everything runs.
    let d = tree(
        "unclaimed-member",
        CFG,
        &[
            (
                "nested/ws/Cargo.toml",
                "[workspace]\nmembers = [\"m\", \"m2\"]\n",
            ),
            (
                "nested/ws/m2/Cargo.toml",
                "[package]\nname = \"m2\"\nversion = \"0.1.0\"\n",
            ),
            ("nested/ws/m2/src/lib.rs", "\n"),
        ],
    );
    assert_run_all(&classify(&d, "nested/ws/Cargo.lock"), "claimed by no class");

    // 3. A path dependency into nested/ws builds with a's root lock; with no
    //    class rooted in nested/ws, its lock is reached by no walk.
    let cfg = "[class.a]\ncargo = [\"a/Cargo.toml\"]\n[class.h]\nhand = [\"other/\"]\n";
    let d = tree(
        "path-dep",
        cfg,
        &[(
            "a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nm = { path = \"../nested/ws/m\" }\n",
        )],
    );
    assert_run_all(
        &classify(&d, "nested/ws/Cargo.lock"),
        "no class's Cargo walk",
    );

    // A member list reaching outside the workspace directory.
    let d = tree(
        "member-outside",
        CFG,
        &[(
            "nested/ws/Cargo.toml",
            "[workspace]\nmembers = [\"m\", \"../x\"]\n",
        )],
    );
    assert_run_all(
        &classify(&d, "nested/ws/Cargo.lock"),
        "outside its directory",
    );
}

// rivet: verifies REQ-027
#[test]
fn bazel_and_nix_forms_the_cold_review_found_select_everything() {
    for (name, file, text, lock) in [
        (
            "lock-glob",
            "nested/ws/BUILD.bazel",
            "exports_files(glob([\"*.lock\"]))\n",
            "nested/ws/Cargo.lock",
        ),
        (
            "concat",
            "MODULE.bazel",
            "L = \"//nested/ws\" + \":Cargo.lock\"\n",
            "nested/ws/Cargo.lock",
        ),
        (
            "included-module",
            "deps.MODULE.bazel",
            "lock = \"//standalone:Cargo.lock\"\n",
            "standalone/Cargo.lock",
        ),
        (
            "nix",
            "nix/default.nix",
            "cargoLock.lockFile = ../standalone/Cargo.lock;\n",
            "standalone/Cargo.lock",
        ),
    ] {
        let d = tree(name, CFG, &[(file, text)]);
        assert_run_all(&classify(&d, lock), file);
    }
}

// rivet: verifies REQ-027
#[test]
fn an_explicit_member_wins_over_exclude_and_nix_relative_paths_count() {
    // nested/ws lists tool as a member AND excludes it: Cargo keeps it a
    // member, so tool builds with nested/ws/Cargo.lock and claims it.
    let cfg = format!("{CFG}[class.t]\ncargo = [\"nested/ws/tool/Cargo.toml\"]\n");
    let d = tree(
        "member-beats-exclude",
        &cfg,
        &[
            (
                "nested/ws/Cargo.toml",
                "[workspace]\nmembers = [\"m\", \"tool\"]\nexclude = [\"tool\"]\n",
            ),
            (
                "nested/ws/tool/Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n",
            ),
            ("nested/ws/tool/src/main.rs", "fn main() {}\n"),
        ],
    );
    let v = classify(&d, "nested/ws/Cargo.lock");
    assert_eq!(v["explain"]["t"][0]["via"], "cargo-lock", "{v}");

    for (name, file, text) in [
        (
            "nix-same-dir",
            "nested/ws/default.nix",
            "cargoLock.lockFile = ./Cargo.lock;\n",
        ),
        (
            "nix-relative",
            "other/pkg.nix",
            "lockFile = ../nested/./ws/Cargo.lock;\n",
        ),
    ] {
        let d = tree(name, CFG, &[(file, text)]);
        assert_run_all(&classify(&d, "nested/ws/Cargo.lock"), file);
    }
}
