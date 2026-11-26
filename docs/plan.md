# sieve plan

## Step 1: bootstrap (done in this commit)

rivet 0.43.3 pinned through varve (realm pulseengine, layer 2026.11.43).
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
tag. Owner option dialog (2026-11-20): they move to v0.4.0, with REQ-022
(its only evidence is the author-run DD-009). `rivet release status v0.3.0`:
cuttable.

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

The first v0.3.0 tag on f93b113 was withdrawn before its release run started
(run 44710986174 cancelled while queued; nothing published): the fleet stopped
Intel macOS builds (owner, 2026-11-20), so the release drops
x86_64-apple-darwin and publishes 3 archives. v0.3.0 is re-tagged on the
commit that makes that change.

## v0.4.0 (prepared 2026-11-22)

Shipped requirements:
- REQ-033 `classify --format github-output --record <file>`: the JSON verdict
  in the same call as the outputs, accepted by check-workflows gate (b)
  (gale's switch, relay's pilot; sieve#25, implemented);
- REQ-034 check-graph fails a `hand` entry that matches no tracked file
  (sieve#23, implemented);
- REQ-032 every lockfile committed, resolved `--locked` and audited, with
  planted controls in the Test job (implemented on CI evidence);
- gate (a) refuses `paths:` on any trigger, push included; `equivalence
  --map` takes the union of outputs mapped to one class (gale, sieve#25);
- REQ-027: the lock beside an implicit workspace root (no `[workspace]` in or
  above the package) belongs to its class (jig replays: gale #450,
  pulseengine.eu #402);
- leaf scoping names a blobless or shallow checkout in its reason (sieve#27 C);
- wasmtime differential pinned to v50.0.3 (RUSTSEC-2026-0587).

Owner decision (2026-11-22, relayed by pulseengine.eu): cut now, and
FEAT-008..011 and REQ-022 move to v0.5.0, because their fleet criteria
(plant controls per adopter, gale's 30 PRs, kiln, signing) are not met.
`rivet release status v0.4.0`: cuttable.

Open for v0.4.x: sieve#24 (drift wiring), sieve#27 A and B (wait for an owner
decision), signing the WASI component with wsc v0.12.2 (sigil#395 is fixed).

## v0.4.1 (prepared 2026-11-22)

With `[workflows] scope_leaf_edits = true` (owner approved sieve#27 A and B,
2026-11-22, relayed by pulseengine.eu):
- REQ-035: an edit to a workflow file with no classifier, no reusable call,
  no gating `if:`, no `needs` read and no job chain selects no class instead
  of everything; reusable, workflow_run-linked, classifier-hosting, merge-key
  and deleted files still run everything, each with its own control;
- REQ-036: dependabot.yml, CODEOWNERS, FUNDING.yml and PR/issue templates
  under .github/ select only their claiming classes unless any other tracked
  file names them;
- every scoped .github/ edit also selects the classes of gated whole-repo
  linter jobs (local composite actions and reusable workflows inlined from
  parsed YAML); REQ-030 refuses a job that reads `needs` outside `if:`.

check-workflows (sieve#30, relay): required names built from
`${{ matrix.<key> }}` expand per matrix row; a required job with `if:
always()` or `!cancelled()` passes gate (b).

release.yml: backticks in the notes heredoc no longer run as commands (the
v0.3.0 and v0.4.0 notes lost words; v0.4.0's were corrected by hand).

Evidence: the owner's condition for A and B was a security review. A
fresh-context reviewer ran 5 rounds and the automated commit review ran on
each push: 8 confirmed false skips, each fixed with a control that fails
with the fix reverted; round 5 found none. Documented limits: remote
actions and reusable workflows are not inlined; a linter run from a script
needs a .github/ claim; REQ-030 does not see a gate passed as an artifact.
`rivet release status v0.4.1`: cuttable.

## v0.4.2 (prepared 2026-11-22)

Two check fixes found by relay on its v0.4.1 pilot (#34):
- check-graph graph-inputs counts a class's `.github/` claim under
  `scope_leaf_edits` (the claim the v0.4.1 README asks for when a script
  reads .github/); without scoping it is still reported as claiming nothing;
- check-workflows gate (b): an `always()`/`!cancelled()` job that needs a
  non-required job passes when a step reads that need's result
  (`needs.<x>.result`, `needs['x'].result`, `toJSON(needs)`) and fails
  `needs-result-unread` otherwise, instead of the `needs-not-required`
  message, which was wrong for that form.

No change to classify. Evidence: a control for each, failing with the change
reverted; fmt, clippy and the full suite on the PR head and on main.

## v0.4.3 (prepared 2026-11-24)

Two classify fixes for gale's L18 lane (#38):
- sieve#36: a local `uses: ./<p>/...` is resolved through an earlier
  same-repo `actions/checkout` with `path: <p>`, so linter-class selection
  and per-job scoping see the real action file. The checkout path is
  modelled only when it is plain (no `${{`, `\` or `..`), has no `ref:`,
  names no other repository, and no later step can rewrite it (a run step
  touching `<p>`, a working-directory under or above it, or a path-like
  action input that names it or cannot be placed). Anything else, and any
  `..` in a local `uses:`, leaves the reference unresolved, which runs
  every class (control: an unresolvable local reference still runs
  everything);
- sieve#37: a job reads `needs` only inside `${{ }}` or an `if:` value; a
  run script mentioning the word no longer refuses scoping. An unclosed
  expression still counts as a read.

Evidence: a control for each refusal, failing with its check reverted; a
fresh-context review (clean at f2e0f8c) and automated security review on every push,
each confirmed finding fixed with its own control; fmt, clippy and the full
suite on the PR head 54ca189 and on main cc3e431.

## v0.4.4 (prepared 2026-11-25)

- sieve#41 (#42, REQ-038): a later `run:` step distrusts a checkout path
  only when one of its commands is a writer and names the path or runs in it.
  0.4.3 counted any file or git word plus the path as a word anywhere
  (a comment, a read, a branch name), so gale scoped 0 of 82 jobs.
  `src/runscan.rs` splits the script into commands. It fails closed on
  unknown variables, expressions, unresolved command names, code piped into
  shells, inline interpreter code naming the path and unparseable scripts.
  Steps in a non-POSIX shell are not parsed: any mention of the path counts.
  It is not a sandbox: a rewrite hidden on purpose is the documented limit.
- sieve#24 (#40, REQ-037): README wiring against config drift (a weekly
  check-graph, observe on full runs, a merged-PR replay), with a planted
  move control.

Evidence:
- a fresh-context adversarial reviewer ran four passes: 21, 12 and 12
  reproduced findings, then 7 groups (mostly false refusals);
- the automated commit security review ran on each of 10 pushes and found
  34 more;
- every finding was fixed with a unit control or documented as a limit;
- the always-true and always-false mutants of `may_write` are killed;
- gale's L18 probe on its real tree, rerun at each head: 74 of 82 jobs
  scoped, 0 writes under gale/ among them, every planted write refused;
- fmt, clippy and the full suite pass on the PR head ae4bf39 and on main
  9ca4a4a.
