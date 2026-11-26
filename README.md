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
sieve classify ... --format github-output --record "$RUNNER_TEMP/verdict.json"  # outputs + JSON verdict, one call
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
check, on any trigger (`check-workflows` gate (a) refuses `push: paths:` too).

To keep the verdict for `sieve observe`, add `--record "$RUNNER_TEMP/verdict.json"`
to the one classify step and upload that file. A second classify step fails
gate (b). The record is written before the outputs; if it cannot be written,
no output is written and classify exits 2, so every gated job runs.

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

### Nested workspace lockfiles (REQ-027)

A `Cargo.lock` beside a manifest that declares its own `[workspace]` pins that
workspace's builds. sieve finds the workspace the way Cargo does: a root whose
`exclude` covers a package is skipped. The root walk of every class whose
package builds in that workspace claims the lockfile. A crate reached only as a
path dependency doesn't, because it builds with its dependant's lock. A change
to the lockfile selects those classes and every class with a claim inside the
workspace directory. It still selects everything when:
- no class's root walk reaches it;
- a member manifest under it is claimed by no class, or the member list
  reaches outside the directory;
- `[run_all]` lists it, or lists the basename `Cargo.lock`;
- a tracked Bazel or Nix file (`BUILD`, `MODULE.bazel*`, `WORKSPACE*`,
  `*.bazel`, `*.bzl`, `*.star`, `*.nix`) may name it, by any of:
  - its path;
  - a label from any package above it;
  - `Cargo.lock` together with its directory;
  - `.lock` in a BUILD or Nix file at or above it;
  - a relative path in a Nix file that resolves to it.

  A crate_universe lockfile feeds Bazel targets that sieve does not trace back
  to it (DD-009).

The repository-root `Cargo.lock` always selects everything.

### Scoped leaf-workflow edits (opt-in, REQ-030)

With `[workflows] scope_leaf_edits = true`, an edit to one file directly under
`.github/workflows/` can select less than everything. In git mode sieve reads
the file at the merge base and at head. If only job bodies changed, it selects
the classes the classifier `if:` reads for:
- the changed and added jobs;
- every job that needs a changed job, transitively;
- every job those need.

An ungated changed job selects nothing, because it runs on every pull request
anyway. The checkout needs the history with blobs (`actions/checkout` with
`fetch-depth: 0` and no `filter: blob:none`): without the workflow at the merge
base, the edit selects everything and the reason says so. The edit still
selects everything when:
- the file is added, deleted or reusable (`on: workflow_call`);
- anything outside `jobs:` changed;
- a job is removed;
- a job running `sieve classify` changed;
- a changed job changed `if`, `needs`, `continue-on-error`, `strategy`, `uses`
  or `outputs`, or a step's `if`/`continue-on-error`;
- a `needs:` entry is not exactly a job id (GitHub matches it ignoring case);
- a step with `continue-on-error` changed;
- an affected job has a step `if:` reading any context but `github` or
  `runner`, or a `strategy` computed from an expression;
- an affected job reads `needs` outside its `if:` and `needs:` (in `env:`,
  `with:` or a `run:` script, which could skip its own work on a classifier
  output); only `${{ … }}` and `if:` values count, so the word in a
  shell comment does not;
- a job in the closure has any `if:` other than the exact classifier form or
  `always()`/`!cancelled()`;
- the file uses a YAML merge key;
- nothing changed once parsed (comments, layout);
- `[run_all]` lists the file.

`--paths` has no base version, so there `.github/` still selects everything.
Composite actions, reusable workflows and everything else under `.github/`
also always select everything, with two exceptions (v0.4.1, sieve#27):

- **A file with no classifier (REQ-035).** When the edit above would select
  everything, but neither version of the file hosts a job running
  `sieve classify`, calls a reusable workflow, has a job `if:` other than
  `always()`/`!cancelled()`, has a step `if:` reading any context but `github`
  or `runner`, has a job that reads `needs` outside its `needs:` list, or has
  a job that needs another (which could pass it a gate as an artifact), it
  selects no class instead: `needs:` stays inside a file, so none of its jobs can be
  gated, and they all run anyway. The `if:` condition covers a classifier sieve
  does not recognise (a wrapper script, an action). Everything still runs when
  either version uses a merge key, has no `on:`, is reusable or triggered by
  `workflow_run`, when another workflow runs on `workflow_run` of it, and when
  the file is deleted.
- **Files GitHub's services read (REQ-036).** `.github/dependabot.yml`,
  `CODEOWNERS`, `FUNDING.yml`, `PULL_REQUEST_TEMPLATE.md` and files under
  `ISSUE_TEMPLATE/` or `PULL_REQUEST_TEMPLATE/` select only the classes that
  claim them, unless `[run_all]` lists them or any other tracked file (a
  workflow, an action, a script, a doc include; the sieve config excepted)
  contains their name.

Every scoped `.github/` edit (REQ-030, REQ-035, REQ-036) also selects the
classes gating jobs that lint the whole repository, and every job those
need: a step whose `run:` or `uses:` (a local composite action included)
names actionlint, yamllint, zizmor, pre-commit, prek, super-linter,
megalinter, checkov, prettier, typos, codespell, reuse, markdownlint, lychee,
check-jsonschema, editorconfig-checker, biome, dprint or taplo. Local
composite actions and local reusable workflows are inlined, recursively (a
`./<p>/...` path after a same-repository `actions/checkout` with `path: <p>`
earlier in the job resolves to the repository's own `./...`); a
local `uses:` that resolves to no tracked file, or sits under a path checked
out from another repository or ref (or rewritten by a later step: see below), or follows
a checkout sieve cannot model (a fork, an expression or `..` path), a linter job gated by an
`if:` that is not the classifier form, or a linter step behind its own
step-level gate makes the edit run everything. Remote actions and remote
reusable workflows are not inlined. A step that rewrites the workspace root (a `git
checkout` of another ref before a local `uses:`) is not detected. An action
input that rewrites a checked-out path is seen only when its key is
path-like (`path`, `dest`, `dir`, `cwd`, `*-dir`, ...). A later `run:` step rewrites the path
when one of its commands is a writer (git other than its read-only
subcommands and options, rm, mv, cp, tar, rsync, curl, an in-place `sed`,
`perl`, `yq` or `awk`, `sort -o`, `docker run`/`cp`, `cmake -E` or its build
and install trees, `gh` downloads and checkouts, a redirection; as the command
name or anywhere after a wrapper such as `sudo -u x` or `timeout 5m`) and names
the path or runs in it. A writer's name as a mere argument (`grep -q zip`,
`west sdk install`) is not a writer; a writer word after an unknown command (`with-device rm`)
counts when a later word names the path outright. Any tool's output flag
(`-o`, `--output`, `--target`, `--prefix`, `--dest`, ...) naming the path
outright counts (`ncc build -o p/dist`). A build directory (`--build-dir`, `cmake -B`) is
a program's own output, not an output flag. Data tools that run another
command (`npm exec`, `yarn dlx`, `go run`, `bazel run`) are scanned like
wrappers. A cwd in the path counts for extractors, clones and downloads
without an output flag, and for any writer with a relative operand. A step whose shell is not bash or sh
(`shell: pwsh`, or a Windows runner without `shell:`) is not parsed: any
mention of the path counts; a `runs-on` expression applies both. The path is named: literally,
through a variable assigned a path naming it (step, job or workflow `env:`,
the script, a `for` loop, an array, an expansion operand), or by running in
it (`cd`, `working-directory:`). Shell comments, reads (`grep`, `cat`,
`-s "$GALE_ROOT"`), branch names and `gh -R owner/repo` do not count
(sieve#41). An unassigned or indirect variable, an expression or `~` in a
written path, a `cd` to one, code piped into a shell, `xargs` feeding a
writer, `sh -c` and `eval` code, inline interpreter code naming the path, and
an unparseable script (an unclosed quote, substitution or here-document)
count as a rewrite; a source path counts like a destination. A program that
writes into the path on its own (`make`, `cargo`, `west`, `source x.sh`) is
not seen. The scanner is built for honest scripts and fails closed on
what it cannot place; it is not a sandbox. A rewrite hidden on purpose (a
program or a downloaded script that writes the path, or code assembled at run
time) is not seen, the same limit as a program that writes on its own. The
workflow edit that adds such a step is itself scoped to the job it edits, so
that job runs on that pull request. Only the linter names above are detected: any other check that reads
`.github/` (a script, a `make` target, another tool) needs its class to claim
`.github/` (`hand = [".github/"]`) when scoping is on. check-graph counts such a claim
under scoping.

Scoping reads gates from the workflow text. A job that receives a gate out
of band (it downloads an artifact the classifier wrote and exits early on it)
is not seen by REQ-030; such a job must carry the classifier `if:` itself.

Scoping decides only what this pull request runs. It does not re-check that an
edited job's gate still covers what the job now builds: an edit that makes a
b-gated job build class a's code is scoped to b, and later a-only changes skip
that job. `check-workflows` gate (d) catches this for `cargo -p` and
`--manifest-path`; the false-skip controls cover the rest.

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

## Counting observed misses

On main (or every Nth merge-queue run), where every job runs anyway, keep
sieve's verdict for the commit's own diff and compare it with what failed:

```sh
sieve classify --config sieve.toml --event pull_request --base HEAD~1 --head HEAD --format json > verdict.json
# after the other jobs: their conclusions, e.g. from the API's
# /repos/{owner}/{repo}/actions/runs/{run_id}/jobs
sieve observe --config sieve.toml --verdict verdict.json --results jobs.json
```

Each job that failed while sieve selected none of its classes
(`[workflows.job_outputs]`) is an observed miss, and `observe` exits 1. It is
a field oracle beside plant's planted controls.

Misses are reported as m of N. N (`compared_skipped_jobs`) counts the jobs
sieve would have skipped that concluded in the full run; a cancelled or
skipped one is listed in `uncompared_skipped_jobs`, never counted.

With `--costs costs.json` it also reports an estimated saving, the median
cost of each skipped job summed. It is an estimate by construction, never a
measurement:

```json
{"schema": "sieve.costs.v1", "merge_queue": false,
 "jobs": {"Zephyr Kernel Tests": 452, "Kani": 38}}
```

A skipped job with no cost entry is listed in `uncosted_skipped_jobs`, not
counted as zero. `merge_queue` decides `skipped_jobs_run_before_merge`.
Without a merge queue, a skipped job first runs after the merge, on push to
main.

## Drift under churn (REQ-037)

A sieve.toml drifts two ways as the tree changes. Too narrow: a path moves
or a job starts reading a path its class does not claim, and a job can be
skipped. Too wide: every PR fails open to run_all, nothing goes red, and the
saving disappears unseen. Three pieces of adopter wiring catch both; none
needs new sieve logic. They assume `sieve` is on PATH (the varve layer or a
verified release archive).

1. A weekly `check-graph` on the default branch. It fails on any refusal,
   including a claim that no longer matches a tracked file and a manifest
   that is gone, so moving a claimed directory without updating sieve.toml
   turns it red within one run (control:
   `moving_a_claimed_directory_turns_the_scheduled_check_graph_red`).

   ```yaml
   on:
     schedule: [{cron: "17 5 * * 1"}]
     workflow_dispatch:
   permissions: {contents: read}
   jobs:
     drift:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - run: sieve check-graph --config sieve.toml
   ```

   It is not a required check: a schedule has no PR to gate, so a red run is
   read from the Actions tab or a notification, not from a merge block.

2. `sieve observe` on the full runs on main (and in the merge queue, where
   there is one), as in [Counting observed misses](#counting-observed-misses).
   A rising m of N is the too-narrow signal in the field.

3. A weekly replay of the last N merged PRs, which reports how many would
   have skipped anything and why the rest ran everything:

   ```sh
   gh pr list --state merged --limit 50 --json number,baseRefOid,mergeCommit \
     -q '.[]|"\(.number) \(.baseRefOid) \(.mergeCommit.oid)"' |
   while read -r n base head; do
     sieve classify --config sieve.toml --event pull_request \
       --base "$base" --head "$head" --format json --explain > "pr-$n.json"
   done
   jq -s 'map(select(.run_all)) | length' pr-*.json        # PRs that ran everything
   jq -s '[.[] | select(.run_all) | .reason // "none" | split(":")[0]] | group_by(.) | map({reason: .[0], n: length})' pr-*.json
   ```

   The fraction that ran everything is the too-wide signal; a drift towards
   all of them shows up as a number instead of as nothing. The base must be
   in the clone (fetch with enough depth), or classify fails open, which the
   replay counts as run_all.

## Verification

`tests/corpus/` is the language-neutral classifier corpus from gale
(`reference/l7-ci-classifier` at 4050000f, 74 cases). `cargo test` runs every
case against the built binary and requires the exact expected set in both
directions. Safety analysis, requirements and design decisions are in
`artifacts/` (rivet).


## Workflow gates

`sieve check-workflows --config sieve.toml` checks a consumer's workflows
against `.github/required-contexts.txt` (exit 0 pass, 1 fail, 2 usage,
3 refused). The file lists one context per line. A line starting with `#`
is a comment, such as a header noting that the file mirrors branch
protection. A file with no context left is refused. The gates are:

- (a) every workflow hosting a required context runs on `pull_request`
  (no `paths:`/`paths-ignore:` key, even empty; no branch or `types:`
  restriction) and on `merge_group`;
- (b) a required job is skipped only through the classifier: its `if:` is
  absent, exactly `always()`/`!cancelled()`, or exactly the form above, wired to a real `sieve classify` step
  with no env override, no step-level `if:` on outputs, no unobserved
  `continue-on-error`, no skip inherited through `needs:`. The
  `[workflows.job_outputs]` map (job to classes) is HAND-MAINTAINED;
  a required name built from `${{ matrix.<key> }}` is expanded per matrix
  row (list keys and `include:`), and the map takes the job under its
  base name (`Test` for `Test (${{ matrix.label }})`);
  a job with `if: always()` or `!cancelled()` that needs a non-required
  job passes only if a step reads that need's result (`needs.<x>.result`
  or `toJSON(needs)`);
- (c) the classifier selects everything on push, merge_group, no base, a bad
  base and an empty diff;
- (d) cross-class builds (REQ-026): a gated job whose run step names a Cargo
  package (`cargo build|run|test|... -p X`, `--package X`,
  `--manifest-path P`) must have classes that claim everything that package
  builds (its closure as a Cargo root, dev-dependencies included); an
  unknown package or an expression fails closed;
- never-skip guard jobs: `[workflows] never_skip_jobs` names jobs a guard
  requires on every pull request (hardware runs, the evidence verifier's
  negative control, proof checks, equivalence jobs); each must have no `if:`
  beyond `always()`/`!cancelled()`, no skippable `needs:`, and a trigger gate
  (a) accepts.

License: Apache-2.0.
