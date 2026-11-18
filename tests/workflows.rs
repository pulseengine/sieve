//! Every CI job keeps fork pull requests off self-hosted runners, and the
//! release workflow signs what it publishes.

use std::fs;
use std::path::Path;

const GUARD: &str = "runs-on: ${{ (github.event_name != 'pull_request' || \
github.event.pull_request.head.repo.full_name == github.repository) && \
fromJSON('[\"self-hosted\",\"linux\",\"x64\",\"light\"]') || 'ubuntu-latest' }}";

// rivet: verifies REQ-016
#[test]
fn every_job_uses_the_fork_guard() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut jobs = 0;
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let text = fs::read_to_string(&p).unwrap();
        assert!(
            !text.contains("pull_request_target"),
            "{} uses pull_request_target",
            p.display()
        );
        if p.file_name().unwrap() == "release.yml" {
            jobs += release_runners(&text);
            continue;
        }
        for line in text
            .lines()
            .filter(|l| l.trim_start().starts_with("runs-on:"))
        {
            jobs += 1;
            assert_eq!(line.trim(), GUARD, "{}: unguarded runs-on", p.display());
        }
    }
    assert!(jobs > 0, "no jobs found: nothing was compared");
}

/// release.yml runs on GitHub-hosted images (see its header), so it must
/// never be reachable from a pull request and never name a self-hosted label.
/// Returns the number of runs-on lines checked.
fn release_runners(text: &str) -> usize {
    let hosted = regex::Regex::new(r"^(ubuntu|macos)-[A-Za-z0-9.-]+$").unwrap();
    // The triggers are the two-space keys under the top-level `on:`.
    let triggers: Vec<&str> = text
        .lines()
        .skip_while(|l| *l != "on:")
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with(' ') || l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("  ")?.strip_suffix(':'))
        .filter(|k| !k.starts_with([' ', '#']))
        .collect();
    assert_eq!(
        triggers,
        ["push"],
        "release.yml may run only on a tag push (signing identity refs/tags/v*)"
    );
    assert!(
        !text
            .lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("self-hosted")),
        "release.yml names self-hosted outside a comment"
    );
    let mut n = 0;
    for line in text.lines().map(str::trim) {
        if let Some(r) = line.strip_prefix("runs-on: ") {
            n += 1;
            assert!(
                hosted.is_match(r) || r == "${{ matrix.os }}",
                "release.yml: runs-on {r:?} is not a hosted image"
            );
        } else if let Some(os) = line.strip_prefix("os: ") {
            assert!(
                hosted.is_match(os),
                "release.yml: matrix os {os:?} is not a hosted image"
            );
        }
    }
    n
}

// rivet: verifies REQ-017
#[test]
fn release_signs_what_it_publishes() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .unwrap();
    // Steps that must come in this order: what enters the sums is produced
    // before the sums; the sums exist before they are signed.
    let order = [
        "scripts/release-check.sh version-guard",
        "scripts/release-check.sh smoke",
        "cargo cyclonedx",
        "sieve.cdx.json \"release-assets/",
        "-docs.tar.gz\" -C docs-out",
        "> release-assets/build-env.txt",
        "sha256sum ./* > SHA256SUMS.txt",
        "actions/attest-build-provenance@",
        "cosign sign-blob",
        "gh release create",
    ];
    let mut at = 0;
    for step in order {
        let i = text[at..]
            .find(step)
            .unwrap_or_else(|| panic!("release.yml: {step:?} missing or out of order"));
        at += i + step.len();
    }
    // Write access exists only in create-release; everything else, the SBOM
    // tool included, runs with the read-only default.
    assert!(
        text.contains("\npermissions:\n  contents: read\n"),
        "release.yml: the top-level permissions are not read-only"
    );
    let publish = text
        .find("\n  create-release:\n")
        .expect("no create-release job");
    for perm in ["contents: write", "id-token: write", "attestations: write"] {
        let hits: Vec<usize> = text.match_indices(perm).map(|(i, _)| i).collect();
        assert!(
            hits.len() == 1 && hits[0] > publish,
            "release.yml: {perm} must appear once, inside create-release"
        );
    }
    assert!(
        !text.contains("--clobber"),
        "release.yml overwrites published assets"
    );
}
