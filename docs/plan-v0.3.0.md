# sieve v0.3.0 plan (draft)

Work in progress; no production-readiness claims. Drafted on 2026-11-19 against
the North Star in pulseengine.eu `data/north-star.toml`. A planner proposed five
bets. A second reviewer then checked them cold, using only the North Star and
the draft. This version applies its findings.

## Ground truth the plan starts from

- No repository runs sieve in CI yet. Saved runner minutes measured in
  production: none.
- gale's PR cost: about 647 runner-minutes per PR (sum of per-workflow medians
  over 3 runs; Zephyr Kernel Tests 452 of it).
- On gale's current layout sieve runs everything on 24 of the last 25 commits.
  14 of those touch `.github/` and 3 a nested `Cargo.lock`.
- No North Star objective measures CI capacity. sieve serves the North Star
  through objective guards and O5, not through a capacity indicator.

## Status (2026-11-20)

- Owner ruling (2026-11-19, late, via pulseengine.eu): bets A, B, D and E go
  straight to work; Bet C stays queued (it narrows REQ-005).
- Owner option dialog (2026-11-20): Bet C approved, start now; REQ-027 in
  v0.3.0; v0.3.0 is tagged after #13, Bet C and REQ-027 have merged.
- Owner option dialog (2026-11-20, second): FEAT-008..011 and REQ-022 move to
  v0.4.0 (their acceptance is decided by adopters); sieve tags v0.3.0 on green.
- Bet C: REQ-030 implemented behind `[workflows] scope_leaf_edits` (opt-in,
  git mode only), with 28 negative-control edits in tests/leaf_scope.rs.
- REQ-027: nested workspace lockfiles are claimed by the Cargo walks that reach
  them; one a Bazel file names still selects everything (tests/nested_lock.rs).
- Bet A: merged, PR #11 (`[workflows] never_skip_jobs`).
- Bet B: sieve's part is docs/planted-defects.md (PD-01..14, WD-01..06).
- Bet E: `cargo build --target wasm32-wasip2` succeeds unchanged (5.2 MB
  component, gix included).

Each bet below says what sieve ships (decided by sieve's own CI) and, separately,
which fleet outcome it enables (decided by other repositories). Each requirement
belongs to exactly one bet.

## Bet A: classes that must never be skipped (serves the O1, O2, O3 and O5 guards)

The guards say:
- a skipped hardware run counts as a failure (O1);
- the evidence verifier's negative control must run (O2);
- required proof checks and the shrinking trust ledger must run (O3);
- a gate's equivalence test must run in CI (O5).

**sieve ships:**
- a `never_skip` list in sieve.toml;
- `check-workflows` refuses (gate b) any job on that list whose `if:` reads a classifier output;
- the classify JSON marks those classes `always`.

**Acceptance:**
- a corpus group and check-workflows plants for each guard kind (hardware, verifier negative control, proof check, equivalence job), each turning the gate red when the job is made skippable;
- 0 of the plants survive.

**Kill:** a guard job in gale, kiln, spar or relay is still skippable after the
check passes.

**Fleet outcome:** each adopter declares its guard jobs before switching.

**Requirement:** REQ-028 (new).

## Bet B: independent per-adopter controls in plant (safety gate for every switch)

**sieve ships:**
- the list of its planted-defect classes (the 10 used in sieve's own potency checks, plus the cold-review cases) as a stable document plant can target.

**Acceptance (plant's check, per adopter: gale, kiln, spar):**
- at least one must-select control per job class, derived from the repository's own tree without `sieve graph`;
- a select-nothing stand-in fails all of them;
- each of the 10 planted defects turns at least one control red;
- any surviving defect blocks that adopter's switch.

**Kill:** controls can only be derived from sieve's own graph output (REQ-015
broken).

**Fleet outcome:** plant runs the controls as a required check on the pinned sieve.

**Requirements:** REQ-015, REQ-012.

## Bet C: a workflow edit stops meaning "run everything" (precondition for any saving)

This deliberately narrows REQ-005 and SSC-003. The owner approved it on
2026-11-20 (needs-decision notice 20261119T221347-ad7ce8, option `scope`).

**sieve ships:**
- a change confined to one leaf workflow file selects the classes of the jobs that workflow defines;
- a change to the classify wiring, sieve.toml, a reusable workflow or a composite action selects everything;
- workflow parsing through rivet-yaml (REQ-025, after rivet v0.43.0).

**Acceptance:**
- the list of classes allowed to narrow is fixed before the run;
- `sieve equivalence` over gale's last 400 commits shows 0 narrower outside that list;
- a negative control holds: a PR that edits a leaf workflow to weaken its own gating still selects everything;
- replaying gale's 25 current commits, at least 5 of the 14 `.github/` commits become selective.

**Kill:**
- fewer than 5 of the 14 become selective under the rule;
- any control shows a pull request steering its own classification (SH-003).

**Requirements:** REQ-030 (new), REQ-025, REQ-027 (nested lockfile scoping).

## Bet D: gale's first observed numbers, and the switch-over gate for the next adopters

**sieve ships:**
- `sieve observe` reads a consumer cost table and reports, per class, skipped class-instances, estimated saved runner-minutes (median cost times skipped classes, an estimate by construction) and observed misses as "m of N", where N is the number of skipped class-instances compared against a full run on main;
- `check-workflows` flags cross-class builds (REQ-026), seeded with more than one pattern (`cargo build|run|test -p X`, `--manifest-path`, a binary path built by another class).

**Acceptance:**
- over 30 merged gale pull requests after its switch, N is at least 30;
- observed misses are 0 of N;
- mean estimated saving is at least 20 runner-min/PR (one threshold, no gap);
- the report states, per adopter, whether a skipped class still runs before
  merge (merge queue: merge_group runs everything) or only after it (gale has
  no merge queue, so its skipped classes first run on push to main);
- the cross-class check fails on spar's pre-fix draft and on each seeded pattern, and passes after the fix.

**Kill:**
- any observed miss;
- mean below 20 runner-min/PR;
- run-all on more than 80% of the 30 pull requests after Bet C.

**Fleet outcome:**
- gale switches (pulseengine-layers #66 first);
- spar and kiln switch only if Bet D's gale result is not killed, each with an equivalence run showing 0 narrower outside its declared list.

Observed misses are also reported against O4's guard (reverted or reopened work),
the one honest O4 link.

**Requirements:** REQ-029 (new), REQ-013, REQ-019, REQ-026.

## Bet E: sieve classify as a signed WASI component under kiln (serves O5)

**sieve ships:**
- the classify/graph core as a WASI component;
- a sieve CI job that runs every paths-mode corpus case and gale's last 400 commits under kiln and under wasmtime;
- JSON byte-identical to the native binary;
- the component signed by the chain, with the signature verified before it runs;
- a `north-star-input.json` release asset (objective O5, one fact for the
  sieve gate; schema in pulseengine.eu `data/north-star/input.schema.json`,
  seam pulseengine.eu#393) listed in the cosign-signed SHA256SUMS.

**Acceptance:**
- the equivalence job runs in CI, and a skipped kiln leg counts as failure;
- a deliberately divergent build goes red;
- no GAP switch exists;
- at least one real gate runs the signed component: sieve's own CI, or gale's classify step;
- a one-bit-flipped component fails signature verification in CI. Today
  `wsc verify-chain` passes such a component (sigil#395), so `signed` is
  reported false until sigil#395 is fixed and this control turns red.

**Kill:**
- kiln cannot run it with a preopened directory;
- the component needs a second, non-sieve program to compute the change set;
- equivalence passes only behind a GAP switch.

**Claim limit:** the component is eligible to count under O5 once O5 has an
indicator script; O5 is unmeasured today.

**Dependencies:** kiln preopens and a pinnable release; sigil#395 fixed for
the `signed` fact; wasmtime is not in the pinned pulseengine layer, so the
differential leg needs a layer that carries it or a pinned direct download.

**Requirements:** REQ-031 (new), REQ-010, REQ-024.

## Order

1. Bet A and Bet B first: they make every switch-over safe.
2. Bet C next: without it, savings on current layouts are near zero, and it needs the owner's decision.
3. Bet D then measures.
4. Bet E is an independent track that slips first if kiln's preopens or a pinnable kiln release are not ready.

Already evaluated and not a bet: bazel-diff (DD-009).
