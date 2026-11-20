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

Step 4 (release) follows release-artifact-pipeline, mirroring plant: the tag
workflow builds the assets, writes SHA256SUMS and signs it with cosign; varve
then adds sieve to the pulseengine layer as a [[tool]].
Consumer switch-over in each repo is gated by the 400-commit old-vs-new
equivalence run (REQ-013).

## Step 3 status (2026-11-18)

Done in feat/classify-cli: items 1-7. Not done: item 8 (check-workflows).

- Corpus: 74/74, exact selected set; json and github-output agree on every
  case; every paths-mode case also passes with .git removed.
- Potency: 10 planted defects (drop cfg_attr path, .incbin, runtime literal
  reads, join chains, package.metadata.component, generate!, path deps; the
  unknown-path rule; the docs namer rule; unresolved forcing) each turn the
  suite red. generate! first survived (the literal scan covers literal
  paths); unit tests for the default wit/ dir and a non-literal path now kill it.
- gale's real tree (reference/l7-ci-classifier at 4050000f) with the
  reference's GATES as sieve.toml: check-graph 2,056/2,056 graph controls and
  355/355 cargo-metadata controls hold.
- Equivalence on gale (reference in GATE_PROBE_FILES mode vs sieve, last 400
  first-parent commits of origin/main, same tree): 399 identical, 0 narrower,
  1 wider. The wider one is explained: without git, sieve walks the tree and
  read an untracked __pycache__/classify-changes.pyc that names the docs
  file; `git grep` only searches tracked files. Safe direction.
- Known gap: the walk (no-.git mode) includes untracked files, so local junk
  can only widen selection, never narrow it.

## v0.1.0 shipped (2026-11-18)

Tag v0.1.0 on d2c7c06; release run 44360884817 success; cosign, sums and
SLSA attestations verified on the published assets. Seam contract:
pulseengine/sieve#3. Layer entry proposed in pulseengine-layers #64.

## v0.2.0 scope (`rivet release status v0.2.0`)

- REQ-024: one binary, gix instead of the git command line; cargo-util-schemas
  for manifests; the cargo-metadata control stays optional.
- REQ-025: rivet-yaml (git tag) for workflow and config YAML.
- REQ-019 observed-miss counter, REQ-020 classify --explain, REQ-021
  unresolved inputs in the JSON, REQ-022 bazel-diff evaluation, REQ-013
  equivalence gate as a tool, REQ-015 plant's oracle, REQ-018 speed.

## v0.2.0 (2026-11-19)

Scope (`rivet release status v0.2.0`: cuttable): REQ-013 `sieve equivalence`,
REQ-018 index-first file listing, REQ-019 `sieve observe`, REQ-020/021
`--explain` and coded unresolved inputs, REQ-024 gix (no subprocess),
REQ-015 accepted on plant#17. Moved to v0.3.0: REQ-025 rivet-yaml (waits for
rivet v0.43.0 with rivet#1289), REQ-022 bazel-diff evaluated on a real
repository (owner: evaluate for real).

Evidence at tag time:
- classifier corpus 74/74 with an empty PATH (no subprocess); 6 cold-review
  regressions; a second cold review of the gix layer: 0 narrowing cases;
- gale, last 400 first-parent commits: v0.1.0 (git CLI) vs v0.2.0 (gix)
  400/400 identical in git mode; `sieve equivalence` against gale's Python
  reference 400/400 identical; negative control narrower on 2 of 30;
- classify time on gale, kiln, spar, witness: 0.07 to 1.32 s (release build);
  kiln all-.md diff 13.47 s before, 1.26 s after;
- cost: stripped macOS arm64 binary 2.37 MB (v0.1.0) to 5.01 MB.

## v0.2.1 (2026-11-19)

Fixes from spar's first switch-over run (`sieve equivalence`):
- a repeated `--map old=class` kept only the last class silently; one old
  output now maps to every class named (a class not in the config is a usage
  error);
- commits that could not be compared are named (`skipped`, with the reason);
- `--accept-narrowing <class>` declares an intended narrowing: listed as
  ACCEPTED-NARROWER, not failing.
REQ-026 (v0.3.0) records spar's finding: a job that builds another class's
binary (`cargo build -p X`) must claim X's inputs.

## v0.3.0 (prepared 2026-11-20)

Scope (plan in docs/plan-v0.3.0.md, bets A to E). Shipped requirements:
- REQ-028 `[workflows] never_skip_jobs` (bet A, verified);
- REQ-029 `observe --costs`, misses as m of N (bet D, verified);
- REQ-026 check-workflows gate (d), cross-class builds (bet D, verified);
- REQ-025 workflows parsed with rivet-yaml v0.43.0 (implemented);
- REQ-030 scoped leaf-workflow edits, opt-in, git mode only (bet C,
  implemented; owner approved 2026-11-20);
- REQ-027 a nested workspace's Cargo.lock belongs to the classes that build
  it (implemented);
- REQ-031 (draft): CI runs the 55 paths-mode corpus cases through the WASI
  component under wasmtime v42.0.1, byte-identical to native; unsigned, not
  under kiln;
- `equivalence --accept-narrowing <class>:<prefix>` (spar).

The bet features FEAT-008..011 carry fleet acceptance criteria (plant
controls per adopter, gale's 30 PRs, kiln, signing) that are not met at this
tag; where they belong is an owner decision (notice 20261120T064025-60a932).

Evidence at prep time:
- `cargo test --no-fail-fast` green on main cafcf4f; Test and rivet validate
  green on #13 and #18 at their merged heads;
- leaf scoping: 28 negative-control edits in tests/leaf_scope.rs select
  everything; on gale 3 of 40 `.github/` commits scope (FEAT-009 kill
  criterion: open);
- nested lockfiles: tests/nested_lock.rs, 11 tests, including Bazel and Nix
  references that keep run-all;
- a new release asset `north-star-input.O5.json` (pulseengine.eu#393): one
  fact, `pass: false` (unsigned, no kiln), inside the signed SHA256SUMS.
