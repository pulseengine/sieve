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
sieve classify --config sieve.toml --paths src/x.rs --explain        # why each class was selected
sieve --version                                                      # names the JSON schemas
```

The classify JSON carries `unresolved`: per class, each input sieve knows it
cannot see, as `{code, input}` (`include-unresolved`, `generate-path-not-literal`,
`symlink-outside`, `bazel-snapshot-stale`, ...); such a class always runs. With
`--explain` it also carries `explain`: per selected class, the changed path, the
claim that covers it and the edge it came from (`cargo`, `include`, `literal`,
`build-rs`, `wit`, `bazel`, `zephyr`, `symlink`, `hand`). git is read in-process
(gix): classify starts no `git` or other process.

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
  `varve-realms.toml`, `.gitmodules`, `flake.nix`, `flake.lock`, `lean-toolchain`,
  and the sieve config itself (built in, not
  removable), plus the config's `[run_all]` additions;
- a path no class claims, unless the config's `[inert]` list or the docs rule
  covers it (a `.md` or `LICENSE*` file that no non-markdown file names);
- any internal failure: the reason starts with `internal:` and a GitHub
  `::warning::` is printed.

With `--paths`, `--paths-file` or `GATE_PROBE_FILES` sieve makes no git call.
When the tree is a git repository it still lists files from the index (read
in-process), so untracked residue (agent worktrees, build logs, bytecode) is
not read. Only when there is no `.git` does it walk the tree, and then the docs
rule also reads untracked files; that can only select more, never less.

A path with a control character, `..`, `.` or an empty component is malformed
and selects everything; log lines escape `%`, CR and LF so a path cannot forge
a workflow command.

Symlinks at or inside a claimed path are followed: a target inside the
repository is claimed too, a target outside makes the class always run.

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

## Switching a repository over

Before a repository's workflows read sieve instead of their current change
detector, compare the two on the repository's own history:

```sh
sieve equivalence --config sieve.toml --old python3 --old-arg tools/ci/classify-changes.py \
  --ref origin/main --commits 400 [--map old_output=class]...
```

For each of the last 400 first-parent commits it runs the old detector with
`GATE_PROBE_FILES` set to that commit's diff, classifies the same paths with
sieve, and compares class by class. Exit 0 means no commit selects less under
sieve and every commit was compared; any narrowing or a failed run of the old
detector exits 1. On gale's history against its Python reference: 400/400
identical.

## Verification

`tests/corpus/` is the language-neutral classifier corpus from gale
(`reference/l7-ci-classifier` at 4050000f, 74 cases). `cargo test` runs every
case against the built binary and requires the exact expected set in both
directions. Safety analysis, requirements and design decisions are in
`artifacts/` (rivet).


## Workflow gates

`sieve check-workflows --config sieve.toml` checks a consumer's workflows
against `.github/required-contexts.txt` (exit 0 pass, 1 fail, 2 usage,
3 refused):

- (a) every workflow hosting a required context runs on `pull_request`
  (no `paths:`/`paths-ignore:` key, even empty; no branch or `types:`
  restriction) and on `merge_group`;
- (b) a required job is skipped only through the classifier: its `if:` is
  absent or exactly the form above, wired to a real `sieve classify` step
  with no env override, no step-level `if:` on outputs, no unobserved
  `continue-on-error`, no skip inherited through `needs:`. The
  `[workflows.job_outputs]` map (job to classes) is HAND-MAINTAINED;
- (c) the classifier selects everything on push, merge_group, no base, a bad
  base and an empty diff.

License: Apache-2.0.
