# sieve

Shared CI path classifier. Per pull request it decides which heavy CI jobs a
change touches. When it is unsure, everything runs (fail open). Each class's
inputs are derived from the build graph (Cargo manifests and sources, Zephyr
modules, Bazel), not hand-listed.

The independent false-skip controls against sieve live in
[plant](https://github.com/pulseengine/plant), by design: the decider and its
check live apart.

## Use

```sh
sieve classify --config sieve.toml --event pull_request --base "$BASE_SHA" --format github-output
sieve classify --config sieve.toml --paths-file changed.txt          # no git call
sieve graph --config sieve.toml                                      # sieve.graph.v1
sieve check-graph --config sieve.toml                                # controls on the real tree
sieve --version                                                      # names the JSON schemas
```

Wire a heavy job only through this `if:` form, so that a crashed, cancelled
or silent classifier still runs the job:

```yaml
if: ${{ !cancelled() && (needs.classify.result != 'success' || needs.classify.outputs.proofs != 'false') }}
```

Never put a workflow-level `paths:` filter on a workflow that hosts a required
check.

## What runs everything

- any event other than `pull_request`; no base sha; a failed or empty diff;
- `.github/**`, `.cargo/**` at any level, `Cargo.lock`, `rust-toolchain*`,
  `BUILD`, `*.bzl`, `MODULE.bazel*`, `WORKSPACE*`, `.bazelrc`, `varve.toml`,
  `varve-realms.toml`, `.gitmodules`, and the sieve config itself (built in, not
  removable), plus the config's `[run_all]` additions;
- a path no class claims, unless the config's `[inert]` list or the docs rule
  covers it (a `.md` or `LICENSE*` file that no non-markdown file names);
- any internal failure: the reason starts with `internal:` and a GitHub
  `::warning::` is printed.

A class with an input sieve cannot resolve (an `include!` built from an
unknown env var, a non-literal `generate!` path, a stale Bazel snapshot)
always runs.

## sieve.toml

```toml
schema = "sieve.config.v1"

[run_all]
prefixes = ["tools/"]            # added to the built-in list

[inert]                          # HAND-MAINTAINED: read by no heavy job
files = ["rivet.yaml"]
prefixes = ["artifacts/"]

[class.proofs]
cargo = ["Cargo.toml"]           # roots: path deps, targets, #[path], include!, ... followed
cargo_glob = ["drivers/*-thin/Cargo.toml"]
zephyr = ["zephyr/module.yml"]
bazel_build = ["proofs/BUILD.bazel"]   # literal srcs only
hand = ["proofs/"]               # HAND-MAINTAINED; `sieve graph` labels these "hand"
true_inputs = ["proofs/sem.v"]   # check-graph: each must select this class via the graph
```

## Verification

`tests/corpus/` is the language-neutral classifier corpus from gale
(`reference/l7-ci-classifier` at 4050000f, 74 cases). `cargo test` runs every
case against the built binary and requires the exact expected set in both
directions. Safety analysis, requirements and design decisions are in
`artifacts/` (rivet).

License: Apache-2.0.
