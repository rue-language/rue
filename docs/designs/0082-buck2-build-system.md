---
id: 0082
title: "Buck2 as the build system"
status: accepted
tags: [build, tooling, process]
feature-flag: null
created: 2026-08-27
accepted: 2026-08-27
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0069", "ADR-0070", "RUE-1819", "RUE-1118", "RUE-316", "RUE-320", "RUE-1505", "RUE-1523", "RUE-1790", "RUE-1818", "RUE-1934"]
---

# ADR-0082: Buck2 as the build system

## Status

Accepted under RUE-1819, 2026-08-27. This ADR is a retroactive record and an
affirmation: Buck2 has been the build system since the project's first weeks,
but the decision itself was never written down — `docs/development.md` states
"Cargo is not the build system" as a fact without a rationale. The 2026-08
architecture review had to reconstruct the justification from its effects;
this document is that reconstruction, ratified.

## Summary

Rue builds with Buck2 through the repository's `./buck2` wrapper. Cargo is
not used to build the workspace; the two leaf `Cargo.toml` files exist only
for sanitizer/allocator special builds. The decision stands on two properties
the repository measurably depends on — corpus results as cacheable build
actions, and one action cache shared across CI runs, worktrees, and machines
— plus enforceable test-tier metadata and non-Rust build-graph nodes. The
standing costs (build-graph glue, a vendored third-party tree, IDE metadata
generation, per-worktree disk pressure) are accepted and named below, so that
future re-evaluation argues against the real ledger rather than a
reconstruction.

## Context

The repository is ~500k lines of Rust across one workspace, developed at high
velocity by a small number of maintainers plus coordinated agent fleets:
parallel worktrees, overnight fix cycles, and a merge queue. That operating
model shapes what the build system must provide:

- **Expensive deterministic test corpora.** The oracle-diff corpora are
  hour-budget actions (`crates/rue-oracle-diff/BUCK`). Before they were
  converted to cacheable build actions, they re-ran at 91–97% of the merge
  queue's critical path (RUE-1118). OSS test runners — Buck2's own test
  protocol included — do not cache test *results*; expressing a corpus run as
  a genrule with a stamp output makes it a content-addressed build artifact,
  after which merge-queue runs converged to full cache hits (`corpus.bzl`).
  Cargo has no analog: nextest has no result cache, so the same design would
  require a hand-rolled stamp store keyed by hand-hashed input manifests.
- **A cache that spans worktrees and machines.** The BuildBuddy action cache
  (RUE-316, `docs/process/build-cache.md`) serves compilation and arbitrary
  actions (corpus stamps, generated twins, fixtures) across CI runs and every
  local worktree. sccache covers compilation only.
- **Enforceable test metadata.** `test_tiers.bxl` proves every owned test
  target carries exactly one tier and no tier is empty (RUE-1523). Tier
  selection is a queryable property of the graph, not an unvalidated filter
  string.
- **Non-Rust graph nodes.** The zig toolchain, test fixtures, and derived
  artifacts such as the Buck-materialized TOML→JSON twin
  (`//crates/rue-toml2json`, RUE-1524) are ordinary build rules with tracked
  inputs.
- **Remote execution as an option.** Full remote execution is supported
  (RUE-320); the wrapper defaults to `--prefer-local`, with
  `--prefer-remote` as an explicit opt-in (RUE-1505). The RE canary keeps the
  option exercised without making it the daily path.

## Decision

Buck2 remains the build system. The `./buck2` wrapper remains the single
entry point; `scripts/rue` remains the task front end. Cargo is not
reintroduced as a peer build path for the workspace.

The properties above are the decision's load-bearing rationale, in order of
weight: corpus-result caching first, the shared action cache second, tier
enforcement and non-Rust nodes third. A future proposal to replace the build
system must show how it preserves the first two or why they are no longer
needed.

## Consequences

### Positive

- Merge-queue wall time does not pay for unchanged corpora: corpus actions
  are cache hits unless their declared inputs change.
- One cache serves every worktree in an agent fleet and CI, so repeated
  builds of the same revision converge instead of multiplying.
- Test tiers are validated structure, not convention (RUE-1523), and
  affected-target selection (btd) narrows PR runs.
- Derived artifacts and toolchains are tracked build steps with declared
  inputs rather than checked-in outputs or setup scripts.

### Negative (accepted costs)

- ~11k lines of first-party `BUCK`/`.bzl` glue plus ~5.8k lines of reindeer
  third-party scaffolding and a ~105 MB vendored crate tree, all maintained
  in-repo.
- rust-analyzer cannot read the Buck graph; `rust-project.json` is generated
  (`gen-rust-project.sh`) and validated separately.
- Corpus actions have a load-bearing input contract: an undeclared input is a
  silent false pass (`corpus.bzl`), and the premerge/corpus tier seam has
  broken twice by the same shape (RUE-1511, RUE-1788). The expressiveness
  that enables the caching also generates this bug class.
- OSS Buck2 has no persistent cross-daemon cache, so each worktree carries
  its own `buck-out`, and that disk is reclaimed by a person removing finished
  worktrees (or `scripts/rue storage reset`). The `./buck2` wrapper only
  refuses to start a build below a 4 GiB free-space floor. An age-based
  cross-worktree reclaim guard was tried first; it caused more incidents than
  it prevented (RUE-1331, RUE-1683) and could not help under fix-cycle load
  (RUE-1790), so RUE-1934 removed it.
- The contributor on-ramp is steeper than a cargo workspace. With the current
  contributor base this cost is small; it grows if the project seeks outside
  contributors, and re-evaluation should weigh it then.

## Open Questions

None blocking. Re-evaluation triggers, should any arrive: a maintained
test-result cache in the cargo ecosystem; the corpus suites shrinking below
the scale where their caching dominates; or a contributor-growth goal that
makes the on-ramp cost primary.

## Future Work

- RUE-1790 asked for finishedness-based storage reclaim in place of the
  age-based guard. RUE-1934 settled it the other way: the guard is gone, and
  a finished worktree's output is reclaimed by removing the worktree.
- RUE-1818: the interpreter-baseline validator question is deliberately out
  of scope here and tracked as its own decision.

## References

- `corpus.bzl` — corpus-as-cacheable-build-action design and its input
  contract.
- `docs/process/build-cache.md` — shared action cache configuration and
  disk lifecycle.
- ADR-0069 (CI work scheduling), ADR-0070 (Rue program build actions) —
  designs that build on these properties.
- 2026-08 architecture review — the reconstruction this record ratifies.
