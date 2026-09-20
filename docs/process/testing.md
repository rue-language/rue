# Testing the compiler

> **Scope: internal/maintainer-oriented.** The short version for contributors
> is in [CONTRIBUTING.md](../../CONTRIBUTING.md) and
> [docs/development.md](../development.md#choosing-tests): iterate with a
> targeted suite, then `scripts/rue fmt` and `scripts/rue test` before opening
> a PR.

This page collects the operational detail behind the test suites: which tier
runs where, what a local run can and cannot tell you, and the known
wall-clock budgets. The tier mechanics themselves are in
[ci.md](ci.md#test-execution-tiers).

## Proportionate validation

1. During implementation, run the smallest focused test that exercises the
   change plus `scripts/rue quick`.
2. Before publication, run formatting and the relevant spec, UI, CLI, or
   architecture-specific suites.
3. Run the broad/full suite once when the change is cross-cutting or
   high-risk. CI is the final full-platform authority.

Read the banner, not the tally. `./test.sh` asserts
`=== TEST SUITE: PASSED ===`; a run the OOM killer or a full disk stopped
prints `=== TEST SUITE: INTERRUPTED (SIG..) ===` instead (RUE-1782). Buck can
return cached results, so a killed run can still print a complete-looking
`Fail 0` line without having finished.

## What a local tier run does not cover

A tier is not a preview of required CI. `test.sh` selects on
`rue_test_tier_premerge`, while both oracle-diff corpora are `tier = "slow"`
and `test (linux-x64-oracle-diff*)` gates the merge queue regardless — so no
local premerge run can report on them, however green it comes back. When
corpus-affecting cases change, run the focused check:

```bash
./buck2 test //crates/rue-oracle-diff:oracle-diff-test
```

The oracle executes structured joins serially through its ordinary call and
exclusive-place writeback path. This compares order-independent callback
results when launch resources are available. A CLI case that observes actual
thread scheduling or availability declares `concurrency_observation` as
`"progress"`, `"resource_exhaustion"`, or `"output_interleaving"`. The corpus
report names that native-only observation explicitly; the native CLI suite
still executes the case and checks all of its assertions. Ordinary join cases
leave the field absent and remain eligible for differential testing.

Adding a CLI or spec case is the ordinary way to land in that gap, not an edge
case: the corpus audit classifies every case, and one reaching a construct the
oracle does not model is a HARNESS FAILURE until its gap is registered in
`crates/rue-oracle-diff/src/model_gaps/`. A `StrBuf`-built path reaches the
unmodeled byte-copy gap immediately, so a new `std.fs` case starts there by
default. The audit prints the exact `Entry::new(...)` line to add, which is
the whole fix — read past "unknown oracle model gap" to it rather than reading
the message as a compiler bug. RUE-1711 cost a red CI round for want of this.

ADR-0097's differential bridge is the other direction: a check no tier runs
at all. It compares the Lean mechanization's exported corpus with the
compiler, the oracle, and native binaries, and it is a `buck2 run` entry point
rather than a test target, because RUE-2241 is the issue that decides whether
CI gates on it — and because it is red today on one case (RUE-2290). Run it by
hand when the mechanization, the corpus, or ownership checking changes:

```bash
scripts/rue lean-bridge            # or: ./buck2 run //:lean-bridge
scripts/rue lean-bridge -- --case cond_drop_affine --report-json /tmp/bridge.json
```

It exits non-zero on any disagreement and names the pair of views that
disagree, never which of them is wrong. `docs/formal/lean/README.md` explains
the corpus it reads.

Native AArch64 execution is validated only by the Linux ARM64 and macOS CI
legs. Locally, inspect cross-target output with `--emit asm`; a one-backend
fix that looks fine on x86-64 will bounce there.

## Deliberate wall-clock budgets

The generated-oracle smoke target has a fixed two-second child-process budget
and can time out under heavy parallel load on a local host. If failures are
timeout-only and unrelated tests are competing for the machine, rerun that
target once in isolation. A clean isolated run is sufficient local evidence;
do not repeatedly rerun the entire suite. A semantic disagreement, compiler
error, or isolated timeout remains a real failure.

The linking test
`platform_native_system_link_cancellation_reaps_child_and_cleans_workspace`
gives cancellation five seconds to answer, measured from the cancel request
rather than from the start of the link, because its mock linker would
otherwise live for thirty seconds and the point of the test is that
cancellation does not wait the child out (RUE-2005). Every other wait in that
module synchronizes on an event and its bound is only a hang guard.

## Buck execution and storage

An unfiltered `scripts/rue test` delegates selection and execution to Buck in
one `buck2 test` invocation. Its standard selection includes the premerge and
slow tiers while leaving stress tests opt-in. Corpus suites are cacheable Buck
build actions; `weight_percentage = 100` keeps two corpus actions from running
together within one Buck daemon/worktree while still allowing corpus work to
overlap non-corpus actions such as unit tests and compiles.

There is no full-suite host lock or cross-worktree test serialization, so
sibling worktrees may run concurrently. The `buck2` wrapper only refuses to
start a build below a 4 GiB free-space floor, and a finished worktree's
`buck-out` is reclaimed by removing the worktree; `scripts/rue storage` reports
and reclaims stale outputs across registered worktrees.

The optional BuildBuddy action cache is one private user config
(`scripts/rue cache install`) that the wrapper links as `.buckconfig.local` in
a worktree on the first build there; `RUE_NO_REMOTE_CACHE=1` skips that for
one command. See [build-cache.md](build-cache.md). Never commit or print its
credential. Full remote execution is supported (RUE-320): the wrapper defaults
to `--prefer-local`, and `--prefer-remote` is an explicit opt-in for
cache-population or RE-debugging runs, not the default local-development
policy.

## Conventions

- Unit tests belong in the relevant crate.
- Use `CompilerSession` artifact queries for compiler integration tests. Use
  `compile_snapshot()` only when the final executable adapter matters.
- UI tests cover diagnostics, warnings, flags, and presentation behavior.
- CLI tests run the real binary against real files and should cover driver,
  ABI, multi-file, platform, and runtime-I/O bugs.
- `known_bug = "RUE-NN"` and `known_bug_on = [...]` are executable xfail
  markers. When fixing a bug, find its cases and remove markers that now pass.
- Specification tests cite paragraph IDs with `spec = ["X.Y:Z"]`. Update the
  specification and traceability whenever language semantics change.
