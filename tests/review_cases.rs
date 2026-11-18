//! False skips found by the cold review of PR #2 (2026-11-18), as
//! regression tests. In each case class `a` reads the changed file and class
//! `b` also claims it, so before the fix `a` was reported false (masked)
//! instead of failing open.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sieve");

fn tree(name: &str, cfg: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("review")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    for (p, body) in files {
        let fp = d.join(p);
        fs::create_dir_all(fp.parent().unwrap()).unwrap();
        fs::write(fp, body).unwrap();
    }
    fs::write(d.join("sieve.toml"), cfg).unwrap();
    d
}

/// (run_all, class a selected) for one changed path.
fn selects_a(d: &PathBuf, path: &str) -> (bool, bool) {
    let o = Command::new(BIN)
        .args(["classify", "--config", "sieve.toml", "--paths", path])
        .current_dir(d)
        .env_remove("GATE_PROBE_FILES")
        .env_remove("SIEVE_FAULT")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    (v["run_all"] == true, v["classes"]["a"] == true)
}

fn assert_selects_a(d: &PathBuf, path: &str) {
    let (run_all, a) = selects_a(d, path);
    assert!(
        a,
        "{path}: class a not selected (run_all={run_all}): a false skip"
    );
}

/// (name, files, changed path)
type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

const ROOT_PKG: &str = "[package]\nname = \"r\"\nversion = \"0.1.0\"\n";
const ROOT_CFG: &str =
    "[class.a]\ncargo = [\"Cargo.toml\"]\n[class.b]\nhand = [\"src/\", \"tests/\", \"testdata/\", \"schema/\", \"shared/\"]\n";

// rivet: verifies REQ-001
#[test]
fn a_claimed_markdown_file_is_an_input_not_docs() {
    let d = tree(
        "md-hand",
        "[class.a]\nhand = [\"specs/\"]\n",
        &[("specs/a.md", "# a"), ("specs/b.txt", "b")],
    );
    assert_selects_a(&d, "specs/a.md");
    let d = tree(
        "md-include-dir",
        "[class.a]\ncargo = [\"c/Cargo.toml\"]\n",
        &[
            (
                "c/Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
            ),
            (
                "c/src/lib.rs",
                "static T: Dir = include_dir!(\"templates\");",
            ),
            ("c/templates/t.md", "# t"),
        ],
    );
    assert_selects_a(&d, "c/templates/t.md");
}

// rivet: verifies REQ-006
#[test]
fn a_bazel_class_follows_its_in_repo_deps() {
    let d = tree(
        "bazel-deps",
        "[class.a]\nbazel_build = [\"app/BUILD.bazel\"]\n[class.b]\nbazel_build = [\"lib/BUILD.bazel\"]\n",
        &[
            ("app/BUILD.bazel", "cc_binary(name=\"app\", srcs=[\"app.cc\"], deps=[\"//lib:core\"])\n"),
            ("app/app.cc", "int main(){}"),
            ("lib/BUILD.bazel", "cc_library(name=\"core\", srcs=[\"core.cc\"], hdrs=[\"core.h\"])\n"),
            ("lib/core.cc", "int c;"),
            ("lib/core.h", "int c;"),
        ],
    );
    assert_selects_a(&d, "lib/core.cc");
    assert_selects_a(&d, "lib/core.h");
}

// rivet: verifies REQ-006
#[test]
fn root_package_module_edges_are_claimed() {
    let cases: &[Case] = &[
        (
            "inline-mod",
            &[
                ("Cargo.toml", ROOT_PKG),
                ("src/main.rs", "mod a { mod b; }"),
                ("src/a/b.rs", ""),
            ],
            "src/a/b.rs",
        ),
        (
            "test-mod",
            &[
                ("Cargo.toml", ROOT_PKG),
                ("src/lib.rs", ""),
                ("tests/it.rs", "mod common;"),
                ("tests/common/mod.rs", ""),
            ],
            "tests/common/mod.rs",
        ),
        (
            "two-mods-one-line",
            &[
                ("Cargo.toml", ROOT_PKG),
                ("src/main.rs", "mod a; mod b;"),
                ("src/a.rs", ""),
                ("src/b.rs", ""),
            ],
            "src/b.rs",
        ),
        (
            "attr-mod",
            &[
                ("Cargo.toml", ROOT_PKG),
                ("src/main.rs", "#[cfg(unix)] mod a;"),
                ("src/a.rs", ""),
            ],
            "src/a.rs",
        ),
    ];
    for (name, files, path) in cases {
        let d = tree(name, ROOT_CFG, files);
        assert_selects_a(&d, path);
    }
}

// rivet: verifies REQ-006
#[test]
fn literals_without_a_slash_and_deleted_paths_are_claimed() {
    let d = tree(
        "build-rs-dir",
        ROOT_CFG,
        &[
            ("Cargo.toml", ROOT_PKG),
            ("src/lib.rs", ""),
            (
                "build.rs",
                "fn main() { println!(\"cargo:rerun-if-changed=schema\"); }",
            ),
            ("schema/s", "x"),
        ],
    );
    assert_selects_a(&d, "schema/s");
    let d = tree(
        "testdata-join",
        ROOT_CFG,
        &[
            ("Cargo.toml", ROOT_PKG),
            ("src/lib.rs", ""),
            ("tests/t.rs", "#[test] fn t() { std::fs::read(Path::new(\"testdata\").join(\"a.txt\")).unwrap(); }"),
            ("testdata/a.txt", "x"),
        ],
    );
    assert_selects_a(&d, "testdata/a.txt");
    // shared/x.json is deleted by the change, so it does not exist at head.
    let d = tree(
        "deleted-read",
        ROOT_CFG,
        &[
            ("Cargo.toml", ROOT_PKG),
            (
                "src/lib.rs",
                "fn f() { std::fs::read(\"shared/x.json\").unwrap(); }",
            ),
        ],
    );
    assert_selects_a(&d, "shared/x.json");
}

// rivet: verifies REQ-006
#[test]
fn zephyr_cmake_relative_sources_are_claimed() {
    let d = tree(
        "zephyr-rel",
        "[class.a]\nzephyr = [\"zephyr/module.yml\"]\n[class.b]\nhand = [\"drivers/\"]\n",
        &[
            ("zephyr/module.yml", "name: m\nbuild:\n  cmake: zephyr\n"),
            (
                "zephyr/CMakeLists.txt",
                "zephyr_library_sources(../drivers/foo.c)\n",
            ),
            ("drivers/foo.c", "int f;"),
        ],
    );
    assert_selects_a(&d, "drivers/foo.c");
}

/// (run_all, class a selected) helper above; this pins the other direction:
/// widening a claim must not swallow a path no class reads. Claiming whole
/// target or dep-package directories did exactly that (71 of gale's last 400
/// commits selected less than the reference).
// rivet: verifies REQ-004
#[test]
fn an_unclaimed_file_next_to_a_target_or_in_a_dep_package_still_fails_open() {
    let d = tree(
        "no-mask-root",
        "[class.a]\ncargo = [\"Cargo.toml\"]\n[class.b]\nhand = [\"zz/\"]\n",
        &[
            ("Cargo.toml", ROOT_PKG),
            ("src/lib.rs", ""),
            ("benches/b.rs", "fn main() {}"),
            ("benches/other/run.sh", "echo"),
        ],
    );
    assert!(
        selects_a(&d, "benches/other/run.sh").0,
        "benches/other/run.sh must run everything"
    );
    let d = tree(
        "no-mask-bazel",
        "[class.a]\nbazel_build = [\"app/BUILD.bazel\"]\n[class.b]\nhand = [\"zz/\"]\n",
        &[
            (
                "app/BUILD.bazel",
                "cc_binary(name=\"app\", srcs=[\"app.cc\"], deps=[\"//lib:core\"])\n",
            ),
            ("app/app.cc", ""),
            (
                "lib/BUILD.bazel",
                "cc_library(name=\"core\", srcs=[\"core.cc\"])\n",
            ),
            ("lib/core.cc", ""),
            ("lib/stray.txt", "x"),
        ],
    );
    assert!(
        selects_a(&d, "lib/stray.txt").0,
        "lib/stray.txt must run everything"
    );
}
