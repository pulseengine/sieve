# sieve plan

## Step 1: bootstrap (done in this commit)

rivet 0.42.2 pinned through varve (realm pulseengine, layer 2026.11.23).
STPA: 4 losses, 6 hazards, 6 constraints. STPA-Sec: 2 losses, 3 hazards,
3 constraints. 18 top-level requirements, each linked to a constraint;
5 design decisions. `rivet validate`: PASS, 0 warnings.

## Step 3: the CLI against the corpus

Spec sources: gale `reference/l7-ci-classifier` at 4050000f (corpus, 74 cases;
classify-changes.py and check-required-contexts.py as reference only, never
shipped), rivet `reference/classifier-prototype` at 1962b5b3, and the
interface drafted with plant.

1. Vendor the corpus into `tests/corpus/` (DD-004) and write the harness
   first. Baseline: 0 of 74 passing.
2. Core types and config (DD-005): sieve.toml parser, Verdict, the
   sieve.classify.v1 and sieve.graph.v1 serialisers, `--version` with the
   schema versions (REQ-011).
3. Fail-open shell (DD-003): event, base, diff and probe handling; the
   panic and error choke point. Targets cases 024-046, 050-051, 064.
4. Graph walk (DD-002): manifests, path deps, targets, source scans
   (path attrs, cfg_attr, include family, generate! with nested braces,
   build.rs literals and `..` joins, CARGO_MANIFEST_DIR concat,
   package.metadata.component, .incbin), Zephyr module, Bazel literal srcs.
   Targets the remaining classify cases including the six the Python
   reference fails (059-063, 067).
5. Docs and inert rules with a single basename index (REQ-018).
6. check-graph and the check-mode cases (068-074: bazel-srcs, depinfo).
7. No-.git mode for `--paths-file` and `graph` (REQ-010), tested by running
   the paths-mode corpus cases in a tree with no .git.
8. The workflow gates (a)-(c) as `sieve check-workflows` (REQ-014): a port
   of check-required-contexts.py with an equivalence test against it on
   gale's and relay's workflows. Scheduled after the first release unless
   a consumer needs it to switch over.

Exit for step 3: 74 of 74 corpus cases pass, every case selecting exactly
its expected set; `cargo test --no-fail-fast` green in CI on the
self-hosted light runner behind the fork guard.

Step 4 (release) follows release-artifact-pipeline, mirroring plant.
Consumer switch-over in each repo is gated by the 400-commit old-vs-new
equivalence run (REQ-013).
