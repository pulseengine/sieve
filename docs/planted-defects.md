# sieve planted-defect classes (for plant's per-adopter controls)

Work in progress. This is the stable list v0.3.0 Bet B ships (FEAT-008,
REQ-015). plant builds independent controls per adopting repository (gale,
kiln, spar) and checks that each class below turns at least one control red.
IDs are stable: a class is never renumbered or reused, and a retired class
stays listed as retired.

## How a control is shaped

A control is a pair: a path in the adopter's tree and a job class that must
be selected when only that path changes. plant derives the pair from the
repository's own files: manifests, sources, the workflow that runs the job.
It must not derive it from `sieve graph` output (REQ-015). The control
passes when `sieve classify --paths <path>` selects the class. A
select-nothing stand-in for sieve must fail every control.

A class below is "covered" for an adopter when at least one of that
adopter's controls uses the input shape named in its row. An adopter with no
input of that shape (for example no Bazel) records the class as "not
applicable", with the search that found none.

## Classes

| ID | What sieve must do | Input shape a control must use | Where sieve plants it |
|---|---|---|---|
| PD-01 | Follow `#[cfg_attr(..., path = "...")]` module paths | a Rust file reached only through a cfg_attr path | sieve suite |
| PD-02 | Follow assembler `.incbin` (an `include_bytes!`/`include_str!` path is a literal, so PD-03) | a binary file pulled in by `.incbin` | sieve suite |
| PD-03 | Treat a literal path read at runtime by tests or build scripts as an input | a fixture file named by a string literal in a test | sieve suite |
| PD-04 | Resolve chained path joins (`dir.join("a").join("b")`) | a file reached only through a join chain | sieve suite |
| PD-05 | Read `[package.metadata.component]` targets (WIT deps) | a `.wit` file under a component's target | sieve suite |
| PD-06 | Follow `wit_bindgen::generate!` paths, including the default `wit/` dir | a `.wit` file read by `generate!` with no path argument | sieve suite, unit |
| PD-07 | Follow Cargo path dependencies | a file in a crate another class depends on by path | sieve suite |
| PD-08 | Run everything for a path no class claims (the unknown-path rule) | an unclaimed file anywhere, including next to a target and inside a dependency package | sieve suite, tests/review_cases.rs |
| PD-09 | Classify only real docs as docs (the docs namer) | a `.md` file a class claims or a build reads | sieve suite, tests/review_cases.rs |
| PD-10 | Run everything when the graph walk cannot resolve an input | a manifest or include sieve cannot parse | sieve suite |
| PD-11 | Follow a Bazel target's in-repo deps | a file in a package another class's target depends on | tests/review_cases.rs |
| PD-12 | Claim root-package module edges (`mod x;` in a root crate) | a module file of the root package | tests/review_cases.rs |
| PD-13 | Claim slash-free literals and deleted paths | a deleted file, and a file named without a `/` | tests/review_cases.rs |
| PD-14 | Resolve Zephyr CMake relative sources | a source listed relative to a `CMakeLists.txt` | tests/review_cases.rs |

## Workflow-side classes (check-workflows)

These are checked on the adopter's workflows, not by classify controls. plant
may cover them by running `sieve check-workflows` on a planted copy of the
adopter's workflow directory.

| ID | What check-workflows must refuse | Sieve's own plant |
|---|---|---|
| WD-01 | A required workflow that may not report (path filters, missing pull_request or merge_group) | tests/workflows_gates.rs (gate a) |
| WD-02 | A skip that does not come from the classifier's gated `if:` form | tests/workflows_gates.rs (gate b) |
| WD-03 | A skip through needs, steps or a matrix | tests/workflows_gates.rs (gate b) |
| WD-04 | A classifier step other than exactly `sieve classify` | tests/workflows_gates.rs (gate b) |
| WD-05 | A classifier that does not fail open on push, merge_group, no base, a bad base or an empty diff | tests/workflows_gates.rs (gate c) |
| WD-06 | A `never_skip_jobs` guard job that can be skipped or emptied | tests/never_skip.rs (REQ-028) |

## What this list is blind to

- Defects in an adopter's own sieve.toml, such as a class that does not claim
  a file its job reads. The 400-commit equivalence run (REQ-013) and
  `sieve observe` (REQ-019) cover that. This list does not.
- Cross-class builds, where one class's job builds another class's code
  (REQ-026). That check is Bet D work and not yet a class here.
- A nested workspace's `Cargo.lock` (REQ-027). Today it is unclaimed, so
  PD-08 runs everything for it. It becomes a class when REQ-027 ships.
