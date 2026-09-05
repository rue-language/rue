# Required CI

The `CI` workflow (`.github/workflows/ci.yml`) supplies one stable required
check: **CI success**. Branch protection should require that displayed context,
not matrix children. The aggregate uses `if: always()` and fails unless every
expected dependency has the event-appropriate result. Matrix jobs can
therefore be reshaped without changing the protected context.

## Triggers

`CI` runs on `pull_request` and `merge_group` (plus `workflow_dispatch` for
manual re-validation). There is deliberately no `push: [trunk]` trigger
(RUE-1006): trunk only advances through the merge queue, and the merge-group
run's checks are attached to the exact commit that lands, so a post-merge
trunk run would re-test an identical tree. Do not re-add a push trigger;
Valgrind and ASan are part of this same CI dependency graph, so they cannot
complete outside the aggregate gate. Manual CI dispatch retains the
`large_program` selector for expanded Valgrind coverage.

Pull-request runs provide early feedback and may intentionally deselect
unaffected heavy corpus jobs. The merge-group run is authoritative: affected
target selection expands to the full premerge corpus, and the merge-group-only
remote-execution canary must succeed. On pull requests and manual dispatch,
that canary is intentionally `skipped`; the aggregate accepts that one exact
event-specific skip and rejects every other skipped, cancelled, or failed
dependency.

`scripts/ci-required-results.py` is the single expected-job inventory consumed
by the aggregate evaluator and structural validator.
`scripts/validate-ci-gate.py` pins the workflow jobs, aggregate `needs`, stable
displayed name, event-specific remote-execution rule, sanitizer inclusion, and
platform responsibility matrix. Its independent `CI contract` job has no
dependencies, always runs, and itself feeds `CI success`. Removing or renaming
a job without updating every reviewed contract edge therefore fails closed
instead of silently shrinking coverage.

## Scheduled workflows and their failure signal (RUE-1507)

Workflows on a `schedule:` trigger have no audience. No pull request waits on
one, nothing is blocked when one goes red, and the result is a row in a tab
nobody opens. `correctness-repetitions.yml` failed on *every* scheduled run it
ever had — each dead in 18 seconds on an argument the invoked script does not
accept — and that went unnoticed for eleven days. A safeguard that fails
silently is worth less than no safeguard, because its absence would be noticed
while its presence is assumed.

`scripts/check-scheduled-workflows.py` runs in the `CI contract` job on every
pull request. It is deliberately **not** itself a scheduled workflow: the
problem being solved is that unattended signals are not read, so reporting on
another unattended timer would inherit the bug. Required CI is the one signal
in this repository that provably reaches a human.

### What blocks, and what only warns

Exactly one condition fails the build: **a workflow that has run on its
schedule at least twice and has never once succeeded.** That signal is durable
(no flaky run produces it), unambiguous (nothing it protects was ever
protected), and self-clearing (one green run ends it permanently). It is the
RUE-1507 shape precisely.

Everything else warns and exits 0, because this gate runs on every pull request
in the repository — a false positive here blocks *all* work, which is worse
than the bug being defended against. That covers staleness, a workflow GitHub
has disabled, a schedule that has not yet fired, an unreachable or rate-limited
API, and any response the classifier cannot make sense of. The repository's
established posture for an unavailable capability is to say so and continue
(see the BuildBuddy provisioning steps in `ci.yml`), not to halt every merge.

Staleness is advisory for a concrete reason. It is a heuristic over cron
cadence, and GitHub's scheduler jitter, queue delay, and a workflow's own
designed red runs all move it. A `0 6 * * *` cron in this repository starts
anywhere between 06:36 and 07:14 UTC; runs take 10–25 minutes; and a run enters
the `status=success` filter only when it *completes*. An earlier revision of
this gate used a four-period budget read from run creation time, which on real
`release.yml` history came within thirteen minutes of blocking the repository.

The structural checks below *do* block, because they are deterministic
questions about the tree with no external state that could make them
spuriously true.

### Declared policies

A workflow that is genuinely broken with a fix already tracked is declared in
that script's `POLICIES` with the issue that owns it, which is the
workflow-level form of the repository's `known_bug = "RUE-NN"` xfail markers.
The declaration is checked for shape: it must name a real `RUE-NN` issue and
carry a reason, and it must match a workflow that still exists.

When the workflow becomes healthy the waiver is reported as no longer needed
and should be deleted, but that report does **not** block — a workflow turning
green is good news, and good news arriving on a cron nobody chose must not stop
every merge until someone edits a file.

`fuzz.yml` has its staleness assessment disabled outright rather than widened.
It is red by design whenever it finds a crash and already files each one into
Linear (RUE-802, `scripts/fuzz-report-failure.py`). Its full retained history —
226 scheduled runs from 2026-01-01, measured 2026-08-14 — is 67.7% failures,
with a longest failing streak of 74 runs and a longest gap between successes of
75 days. No threshold separates that from a fuzzer that has stopped working, so
this does not pretend one does.

### Keeping the gate honest

The step and its `actions: read` scope are both pinned by
`scripts/validate-ci-gate.py` as executable lines rather than substrings, so
neither can be deleted — nor satisfied by a comment mentioning them — without
failing required CI. Discovery is by `schedule:` trigger rather than a list, so
a workflow added later is covered automatically; a workflow whose triggers the
parser cannot read is *reported* rather than skipped, since silently auditing
nothing is the same failure at per-workflow granularity. The classifier, the
API client, and the transport's error handling are pinned against mocks by
`//:scheduled-workflow-tool-tests`.

### Failure-log artifacts

Scheduled lanes that run commands through `scripts/ci-timed` must also upload
the preserved `rue-ci-failed-logs` artifact on failure. They need it more than
pull-request lanes do, not less: nobody is watching when a weekly job goes red,
so the first person to look arrives days later, by which time the Actions job
log has been truncated to a tail or aged out entirely. The rule applies only to
workflows that actually use `ci-timed` — elsewhere the artifact would always be
empty, and an empty artifact that looks like coverage is the same class of
problem. It is a whole-file check with comments stripped: it proves the upload
exists, not that every `ci-timed` job has its own.

## Platform responsibility matrix

| Owner | Required responsibility |
| --- | --- |
| Linux x86-64 | Complete target-independent premerge suite, all CLI shards, specification corpus, scaling matrix, release smoke, reproducibility, lint/metadata gates, Valgrind, and ASan |
| Linux ARM64 | Native compiler/linker build; compiler, codegen, linker, runtime/archive, runtime ABI, allocator, and target unit tests; every applicable `only_on` spec/CLI case; real ABI, linker, and filesystem CLI programs |
| macOS ARM64 | The same native responsibilities on Mach-O/macOS, including host-conditional compiler/archive tests, every applicable `only_on` case, native linker/runtime execution, and output publication |
| Linux cross-backend step | Explicit host-independent x86-64 and AArch64 compilation/encoding unit coverage |

Linux ARM64 and macOS ARM64 do not repeat the specification corpus. Through
`scripts/run-native-platform-corpus.sh` they register only the `only_on` cases
of the manifest-driven corpora — but that is not the whole of what they run
from those corpora: the three explicit `scripts/rue cli` filters below use the
developer entry point, which sets neither `RUE_CLI_CASE_TIER` nor
`RUE_PLATFORM_CASE_SELECTION`, so every case matching `abi`, `cli.linker`, or
`cli.fs_file_io` runs whatever its tier and whether or not it declares
`only_on`. The ABI selection exactly skips
`cli.differential_opt::aggregate_abi_across_opt_levels`: that one release-smoke
case was an accidental substring match, while the other 61 ABI-named cases are
the native responsibility. That is the responsibility matrix working as
written, and the duplication gate scores it as a declared allowance rather
than leaving it to this paragraph. Backend encoding logic is still tested
explicitly for both architectures on Linux, while native ARM64 lanes prove the
host ABI, object/linker path, runtime archive, syscalls, and platform behavior
that cross-compilation cannot.

They do not repeat the broad compiler test selection. Platform scope is a
validated target label (`rue_platform_native`) attached by the shared Buck
wrappers, and the native lanes query that label from the live graph. The focused
`rue-compiler-platform-native-test` target owns the compiler's host-conditional
assertions by selecting the `#[ignore = "platform_native_ ..."]` rows whose
names carry the `platform_native_` prefix; the target-independent compiler test
selection remains in linux-premerge. A new compiler host assertion self-enrolls
by following that ignore-and-prefix convention. A new native unit target
self-enrolls through its graph attribute. Neither requires a workflow or
validator target-list edit. The fake-tool shell suites (`//:wrapper-script-tests`
and its siblings) carry the same label so the macOS lane runs them under the
stock Bash 3.2; the repetition is declared in
`scripts/validate-test-duplication.py`. So is
`//crates/rue-c-abi-matrix:c-abi-matrix-test`, the generated C-boundary
conformance matrix: it generates paired C and Rue sources for the host, compiles
the C side with the host `cc`, links through it, and runs the result, so each
psABI row — SysV AMD64, AAPCS64, and the Apple arm64 amendments — is proven only
on the lane where that row is native. It reports every trial as ignored on a
host with no `cc`. See `docs/notes/ffi-abi-conformance-audit.md`.

The native lanes set `RUE_PLATFORM_CASE_SELECTION=native` through
`scripts/run-native-platform-corpus.sh`. Both manifest-driven harnesses then
register every case whose nonempty `only_on` list includes the current host;
unscoped target-independent cases and automatic examples are not registered.
This makes new platform-scoped cases self-enrolling. The native lanes also
retain the explicit ABI, linker, and filesystem CLI filters because those
suites contain native-execution cases with empty `only_on` lists. The CI
contract preserves both corpus filters along with the graph-owned
`rue_platform_native` unit selection.

The build jobs use the shared BuildBuddy remote action cache when the
`BUILDBUDDY_API_KEY` secret is available (merge_group runs; fork PRs build
cold) — see `docs/process/build-cache.md` for the availability rules. The
compiler-reproducibility job is the deliberate exception: its two independently
materialized compiler builds stay local and cache-free, while the ordinary
linux-x64 test lane may use BuildBuddy.

Required release coverage is intentionally focused (RUE-1129). The
`release (linux-x64)` job analyzes Buck's configured `rustc_cfg` actions and
fails unless `//platforms:release` supplies `-Copt-level=3 -Clto=thin` while
`//platforms:debug` supplies neither. It then runs `//:release-smoke`, the 29
representative differential-opt cases, through a release-built Rue compiler.
It does not build every crate target or run the exhaustive suite.

`.github/workflows/release.yml` runs the release-configured suite nightly and
on manual dispatch. Its `rue_cli_shard` exclusion avoids repeating the
premerge CLI inventory owned by `//:cli-tests`; `//:cli-tests-slow` still owns
declarative slow sections. The broad release job excludes only the separately
scheduled large-program targets. Dedicated Caldera and Meridian jobs run their
real slow targets nightly with a release compiler, compiling each application
once and reusing the executable across runtime scenarios. Manual dispatch can
select either full program and may select the separate 4x stress tier.

ADR-0067's two performance gates are required but no longer share a job
(RUE-1504). `premerge (linux-x64)` keeps `check-pins`, which asks whether this
tree would stop runs entering their series — decidable from the tree alone, and
about a second of hashing next to a build the lane already paid for.
`performance staleness (linux-x64)` owns the other gate: whether the published
series is *already* stalled. That question is repository-wide rather than
diff-shaped, so the job is ungated by the determinator and takes a
`fetch-depth: 0` checkout, without which it counts no trunk commits and fails
instead of passing (RUE-1258). It stays compile-time only; ADR-0072 Decision 9
deliberately leaves the runtime series out of it, and the CI contract now fails
on a `--runtime-manifest` appearing there, on a `continue-on-error` bypass, and
on the gate reappearing as a premerge step.

The split is a scheduling change, and the gate is listed in
`scripts/ci-required-results.py`, so `CI success` blocks the merge on it exactly
as it did when it was a step. One thing did change, in the safe direction: as a
step it was skipped whenever an earlier step in the premerge job failed (1 of 14
sampled runs), and standing alone it always runs.

It moved because it cost a flat ~156s median on the job that was the run's
longest in 89 of 94 sampled runs — 46% of that job on a pull request touching no
compiler crate. **The new job is not cheap, and is not smaller than the lane it
left.** It pays the gate's own ~156s plus job setup, an ~11s `fetch-depth: 0`
checkout, ~4s of dotslash, and a `rue-bench` build that inside premerge was free
because `Build all targets` had already produced it: **~185–195s** with the
shared action cache, **~250–270s** cold on a fork pull request where no key is
available, against **~152s** for premerge with the step removed. On a cheap pull
request the gate job is therefore the run's longest; on a compiler pull request,
where premerge runs 9–10 minutes, it finishes far inside it. The win comes from
overlap rather than from the work getting cheaper — median run wall time ~308s →
~190s, newly floored by `compiler reproducibility (linux-x64)` at ~181.5s — so
it is a p90 win first and a smaller p50 win. Do not re-derive a saving from an
assumption that this job is free.

**The job then grew to 350–400s, and the reason was not the gate** (RUE-1542).
A step-level breakdown of a 338s run: 6s to build `rue-bench` (a 100% remote
cache hit), 0.6s to fetch both branches, and ~330s to materialize and parse
`performance-data-v1`. The store is append-only and was 1.5 GB across 1,188
records on 2026-08-16, growing ~300 MB a day since protocol-v2 boundary
evidence began collecting on 2026-08-13 — before that it grew ~2.5 MB a day.
The gate's own rules are milliseconds.

So the gate now reads the live epoch instead of the whole store.
`rue-bench staleness-inputs` names each platform's newest-point epoch from
`index.json` — which carries every record's platform, epoch, and finish time,
so the selection never opens a run object — and the step checks out only those
paths. `derive` is unchanged and still derives whatever the data root holds.
Deriving the 69-record selection and the full 1,188-record branch produces
byte-identical gate reports, on the passing case and on the failing one; local
derive time was 197s against the whole branch and 57s against the selection.
The CI contract pins `staleness-inputs` into the job, because this cost returns
as a slow job rather than a failing one and would otherwise be re-added by
anyone restoring the obvious `checkout -- .`.

**This bought a constant, not a trend.** The live epoch grows at the same
~300 MB a day — epoch 6 went from 42 records and 267 MB to 69 records and
419 MB in a day — so the selection is worth less each day it is not paired with
smaller records. The per-sample `boundary_evidence` that drives the growth is
~37.9 KB, or 990 KB for one `startup` workload, and shrinking it is tracked
separately.

Production `debug_assert*` use is governed by
`scripts/validate-debug-assert-policy.py`. The gate is structural (RUE-1525):
`rue_crate`/`rue_binary` emit a premerge-tier `<name>-debug-assert-check` per
crate, scoped to that crate's sources, and `//:debug-assert-ledger-check`
fails when a ledger entry names a crate `rust-project.json` no longer lists —
so a deleted crate cannot leave allowances nothing enforces. Every surviving
call has an exact per-file allowance and rationale; changing the count
requires reviewing the ledger. CFG optimization, code generation, and linking
have no allowances: an invariant whose violation could change emitted code
must be an always-on assertion or a real compiler diagnostic.

## Test execution tiers

Every first-party Buck test target carries exactly one execution-tier label:
`rue_test_tier_premerge`, `rue_test_tier_slow`, or
`rue_test_tier_stress`. The wrappers in `test_defs.bzl` attach those labels to
repository, crate, and toolchain tests; generated crate unit tests default to
`premerge`. A test that belongs outside pre-merge must opt into `slow` or
`stress` at its definition rather than returning success from a skipped body.
Platform-sensitive unit targets may additionally carry the validated
`rue_platform_native` label through the same wrappers. Native lane membership
is an `attrfilter(labels, 'rue_platform_native', ...)` query over the live Buck
graph; the workflow does not carry a parallel target list.
The required Valgrind and ASan jobs are logical `premerge` coverage and remain
explicit jobs inside the CI aggregate: Valgrind provisions an external runtime
tool, while ASan requires a pinned nightly Cargo toolchain that Buck does not
model.

The Valgrind job installs its required runtime through
`scripts/install-valgrind`, not an inline `apt-get` block. Each of `apt-get
update` and `apt-get install` has a 10-minute total bound, 30-second HTTP and
HTTPS acquisition bounds, two apt-native retries, and a 60-second dpkg lock
wait. GNU `timeout` kills the operation's process group after a 30-second grace
period; cancellation cleanup also reaches the timeout process itself, so apt
and dpkg descendants cannot outlive the step. A timeout remains exit 124 and
other nonzero statuses (including 137) are surfaced unchanged. The CI contract
validator pins this wiring and policy so a future workflow edit cannot restore
an unbounded mirror wait or silently change the retry/lock budget.

`./buck2 bxl //test_tiers.bxl:validate` queries Buck's live first-party test
graph and fails unless every target has exactly one tier. Required formatting
CI runs that validation, so adding a raw test rule or conflicting tier label
cannot silently change suite ownership. Vendored `//third-party/...` targets
are excluded because their generated BUCK metadata is upstream-owned.

The same BXL file exposes the canonical named selections:

```bash
./buck2 bxl //test_tiers.bxl:premerge
./buck2 bxl //test_tiers.bxl:slow
./buck2 bxl //test_tiers.bxl:stress
./buck2 bxl //test_tiers.bxl:all
```

Each prints the exact live Buck targets in that selection. The `all` and
per-tier selections omit `rue_cli_shard` scheduling alternatives because the
canonical premerge CLI target already owns their union. CI invokes those shards
explicitly on separate runners.

Use `scripts/rue premerge`, `scripts/rue slow`, `scripts/rue stress`, or
`scripts/rue all` to execute a named selection. `scripts/rue test` retains its
standard full-suite behavior: premerge plus slow, with resource-stress tests
remaining opt in. Filtered `scripts/rue test PATTERN` behavior is unchanged.
Required CI sets `RUE_TEST_TIER=premerge`; `scripts/rue all` exercises the
complete union, while the scheduled release workflow gives the dedicated
large-program targets their own compile-once jobs.

A tier label says when a target runs, not that anything runs it. `validate`
above cannot see CI, so a target moved into a tier no job selects keeps a valid
label while its coverage leaves the merge queue — which is what happened to the
codegen differential (RUE-1117). `scripts/validate-tier-ci-selectors.py`
(`//:tier-ci-selector-validation`, and repeated in the toolchain-free
`CI contract` job) closes that gap: every tier must be selected by a *named* CI
job, registered in the script against the literal selection it relies on.

An unfiltered `//...` run is deliberately not accepted as a selector. The
nightly release sweep runs every tier by accident of not filtering, so it keeps
reporting coverage through the very edit that strands a tier, and it names no
owner. The gate is tier-granular; per-target coverage stays with each suite's
own inventory gate (the RUE-924 audit in `test.sh`, plus the shard planner's
live-graph union assertion).

The exhaustive CLI- and specification-oracle differential harnesses are
`slow`, and are the slow tier's pre-merge selector: `platform-corpus` runs
`//crates/rue-oracle-diff:oracle-diff-test` and `:oracle-diff-spec-test` in two
concurrent linux-x64 lanes (RUE-1117). Both corpora are target-independent, so
one lane each is the whole platform requirement — the nightly release sweep is
release-configuration coverage on top, not the primary signal. Premerge retains
the fixed generated oracle smoke corpus as its bounded codegen canary.
CLI corpus sections may likewise declare `tier = "slow"`.
Automatic examples have the same declarative tier field.
`//:cli-tests-slow` owns those real cases. Mosaic's shipped-program smoke and
17 exhaustive behavior cases are slow, so the four required CLI shards do not
spend their critical path compiling that large program. Caldera and Meridian
instead use dedicated reduced premerge roots plus compile-once slow targets.

The normal 100/1k structural scaling matrix remains a distinct premerge
canary target in the Linux premerge graph;
`//crates/rue-compiler:scaling-matrix-stress-test` enables the real
10k-per-axis ladder and belongs to `stress`. Caldera and Meridian likewise
keep their 4x configurations in dedicated stress targets.

Both scaling-matrix targets wrap the ordinary
`//crates/rue-compiler:rue-compiler-test` test selection and select its `#[ignore]`d
`scaling_matrix_*` rows with `--ignored scaling_matrix`. They do not compile
the crate again. Excluding heavy rows with `#[ignore]` rather than a
target-specific `--cfg` is what keeps a dedicated target from silently
becoming a superset of the unit target it was meant to be separate from — the
defect RUE-1262 found, where the premerge lane ran 813 shared tests twice to
gain three. A test named `scaling_matrix_*` and marked `#[ignore]` is in the
premerge canary; anything else marked `#[ignore]` is opt-in only and no target
runs it.

Execution tier and discovery are separate concerns. A required pre-merge test
may also carry `rue_heavy_suite` or `rue_dedicated_suite` so wrappers can
select it explicitly without misrepresenting it as scheduled-only coverage.
`scripts/rue quick` excludes non-unit integration harnesses via
`rue_not_quick` and dedicated suites such as the structural scaling matrix.
The full local suite still discovers and runs them once. Required CI may set
`RUE_CI_DEFER_DEDICATED_SUITES` to the exact live set of
`rue_dedicated_suite` targets when explicit parallel jobs own them. `test.sh`
fails unless the environment and Buck graph match exactly, so a dedicated
target cannot be silently dropped. The scaling matrix currently remains in the
Linux premerge graph rather than having its own workflow job.

The Linux premerge lane retains broad target discovery and defers its two
heaviest corpora — `//:cli-tests` and `//:spec-tests` — to explicit Linux
`platform-corpus` jobs so those corpora overlap the main lane instead of
serializing behind it (RUE-1115). `test.sh` accepts that deferral only under
`CI=true`, validates each target against Buck's live `rue_heavy_suite` query,
and continues to audit every corpus target it owns. Local full suites never
defer coverage. Native ARM64 lanes use the explicit responsibility matrix
above instead of broad discovery.

Caldera and Meridian are absent from the ordinary CLI corpus by explicit
filters because their complete generated graphs are slow-tier workloads. Each
application instead compiles as a `rue_program` build action (ADR-0070 /
RUE-1405) — a real cached artifact keyed on its declared read closure, served
by the remote action cache across invocations and lanes — and every runtime
scenario is its own `rue_program_test` consuming that artifact. The required
broad pass runs the `//examples:large-example-{caldera,meridian}-canary` scenarios
over reduced roots that exercise each application's core compiler/runtime
path without claiming full-program coverage. Nightly
`//examples:large-example-{caldera,meridian}-slow` suites fan out to the per-scenario
tests over the complete real roots, and the `stress4` configurations live
only in the corresponding `-stress` scenarios, which reuse the same compiled
artifact instead of recompiling it. The positive warm-cache control
(`scripts/check-rue-program-warm-cache.sh`, scheduled in `cache-probe.yml`)
asserts the cross-root cache service this mechanism exists to provide.

Required Valgrind coverage explicitly sets
`RUE_SANITIZER_LARGE_PROGRAMS=none`; it does not quietly recurse around one
large directory while including the other. Manual sanitizer dispatch can
select `caldera`, `meridian`, or `all`. The ASan job instruments the Rust arena
allocator rather than compiled Rue applications, so that selection does not
apply to it.

The CLI cases that name a checked-in root do not compile it. ADR-0070 Phase 2
(RUE-1406) declares each of the nine such roots as a `rue_program`, collects
them into `//examples:cli-staged-programs`, and gives every CLI corpus action that
directory as a declared input; the harness runs the prebuilt executable a case
names. 64 cases work this way. What still compiles inside the harness is
deliberate and structural — the harness stages a case only when the case says
nothing about the compile — so the cross-target `cli-test-fixtures` cases, the
repo-relative `source_path` fixture, the `differential_opt` calculator case,
the one-scenario wordfreq root, and all ~4,050 inline-source cases compile as
before, as do the RUE-48 automatic example smokes. `cases/examples_meridian.toml`
runs in CI again: RUE-1083 disabled its six scenarios because each paid a full
80.7s compile of the same root, and they now share one cached artifact.

The ordinary CLI corpus is additionally split into `CLI_TEST_SHARD_COUNT` (root
`BUCK`) cost-balanced shards, `//:cli-tests-shard-0 .. -N`, so its wall clock
collapses to N parallel slices per platform (RUE-1116/RUE-1158). Each shard
sets `RUE_CLI_TEST_SHARD=k/N`; the harness assigns the longest measured cases
first to the currently lightest shard, with stable name and shard-index
tie-breaking. This makes the assignment deterministic, exhaustive, and
pairwise disjoint while balancing estimated runtime rather than case count.
The checked `crates/rue-cli-tests/shard-weights.json` has common weights plus
optional `linux-x64`, `linux-arm64`, and `macos` overrides. A run rejects an
estimated slowest shard more than 25% above the mean.

Each executed required shard writes `rue_cli_case_timing` JSONL records and
uploads them as a `*-case-timings` artifact. A remotely cached test result has
zero executed cases and therefore no artifact. Combine repeated samples with
the median and update the checked weights deterministically:

```bash
scripts/generate-cli-shard-weights.py \
  --timings linux-x64=linux-x64-1.jsonl \
  --timings linux-x64=linux-x64-2.jsonl \
  --timings linux-arm64=linux-arm64.jsonl \
  --timings macos=macos.jsonl
./buck2 test //:cli-shard-weights-validation
```

Use `common=PATH` to refresh the cross-platform fallback and the default cost
for newly discovered cases. Platform-specific historical samples remain useful
for local sharding experiments even though required target-independent shards
run on Linux. The shards carry both `rue_heavy_suite` (so
`ci-heavy-suite` runs them and the broad pass skips them) and `rue_cli_shard`
(so a local `./test.sh` full run executes the monolithic `//:cli-tests` once
instead of every slice — `test.sh` subtracts the `rue_cli_shard` set from its
heavy-suite discovery). Nothing else re-runs the slices on required CI.
`scripts/plan-cli-shards.py` therefore queries every live `rue_cli_shard`
target, requires their union to be the contiguous count derived from
`ci/cli-shard-planning.json`, and emits the `platform-corpus` matrix. A
graph/count drift fails the planning job rather than dropping a slice; the
protected `CI success` context remains unchanged.

The planning JSON is a versioned, manually reviewed measurement snapshot. Its
run IDs and acquisition instructions trace the native floor, CLI total, and
fixed per-lane items back to Actions jobs and `ci-timed`/`what-ran` evidence.
Refresh the entire cohort and indivisible inventory together as instructed in
the file; the planner validates that provenance is present, but the inventory
is explicitly not a graph-derived completeness proof. Phase 6's
`phase_6_remeasurement` records both the pre-change merge-group execution and
the passing post-change PR execution, including substantive job walls and queue
delay classification. Different events, unmatched cache state, and runner
queueing mean those walls validate execution rather than a causal speedup.

### Correctness hang guards

`crates/rue-cli-tests/cases/execution_contracts.toml` is the single authority
for CLI timeout profiles. `ordinary`, `slow`, and `stress` are generous
correctness hang guards, not promises about compiler speed. Cases select a
named execution contract; raw per-case or per-contract millisecond deadlines
are rejected so one-off budgets cannot drift into the corpus.

The same file defines whole-suite headroom. `scripts/ci-heavy-suite` derives
each shard's executor deadline from that shard's LPT-assigned expected cost,
then adds proportional and fixed headroom and applies a conservative minimum.
This lets a truly stuck compiler fail while leaving loaded CI hosts room to
finish. Mosaic remains in the slow tier and uses the slow hang profile; stress
programs remain opt-in or scheduled.

The weekly Correctness repetitions workflow derives and runs every ordinary CLI
shard multiple independent times. It uploads per-run logs and a summary, continues
after a failure only to gather flake evidence, and exits failed if *any*
repetition failed; a later pass never masks an earlier failure. Required
correctness jobs do not automatically retry failed cases.

Every job in that workflow really executes its corpus — each starts on a fresh
runner with an empty `buck-out` and no cache secret — which is also what makes
it the undeclared-input safeguard for the corpus build actions (RUE-1222): a
corpus that reads a path its action does not declare passes against the previous
tree's result, and only a real execution against the current tree can tell.
Alongside the repeated shards, `execute-every-corpus` runs each remaining
converted corpus once, taking its inventory from `scripts/ci-corpus-inventory`
so a corpus converted later is swept without editing the workflow. The same runs
drive a 20% observed lane-wall skew guard (on the scheduled refresh path, not a
contributor's required run) and are where
`crates/rue-cli-tests/shard-weights.json` gets freshly measured
per-case timings, which feed both shard balance and the derived correctness
deadline `//:cli-timeout-policy-validation` gates on.

Repetition within one workspace needs more than `--no-remote-cache`, which
disables only the remote cache: the index rides `-c rue.corpus_repetition=N` so
each repetition is a distinct action digest. Nothing in this workflow is a
required check, so a failure here turns nothing red — read the run list after
changing it. See `docs/process/build-cache.md`.

## Nothing executes twice (ADR-0069 §2)

**No test the gate can enumerate executes more than once per platform per run
without a declared reason.** `scripts/validate-test-duplication.py`
(`//:test-duplication-tool-tests` pins its logic) is the gate, and it runs as
the `Check nothing executes twice` step of the `premerge (linux-x64)` job.

The qualifier is load-bearing, and "Where it cannot see" below says exactly
where it applies. A gate that claimed more than it checks would be the same
kind of defect it exists to catch.

It exists because every other gate here compares *target lists*.
the shard planner compares BUCK's live shard union with the derived matrix,
`scripts/validate-ci-gate.py` compares jobs with the responsibility matrix, and
`//test_tiers.bxl:validate` compares tier labels with the test graph — so a
target that becomes a strict superset of another is invisible to all of them.
That is how `//crates/rue-compiler:scaling-matrix-test` came to re-run 813 of
`rue-compiler-test`'s tests, in the same lane, to gain three, for weeks, with
every check green (RUE-1262).

How it decides:

- **Lane membership is queried, not transcribed.** The premerge lane is
  `attrfilter(labels, 'rue_test_tier_premerge', set(//... toolchains//...))`
  minus the `rue_cli_shard`, `rue_ci_dedicated_lane`, and
  `rue_ci_clippy_lane` sets; the corpus and gated-lane inventories come from
  `scripts/affected-targets corpus-targets` and `lane-targets`, which are the
  lists CI's determinator already consults. A lane added to `SELECTABLE_LANES`
  and not classified by the gate fails it, so this adds no unwatched row to
  ADR-0069's ledger.
- **Identities come from `--list`.** Each target is listed with the exact args
  and env its own Buck target carries, so the scaling matrix's
  `--ignored scaling_matrix` selection lists three tests and the CLI shards
  list their own slices. `#[ignore]`d tests are subtracted from `rust_test`
  binaries, because an ignored test is not scheduled work.
- **Whether a target can be listed is decided before anything runs.** The
  `--list` protocol is not universal and probing for it is not free:
  `scripts/test-reproducible-output.sh` and
  `scripts/oracle-diff-generated-smoke.sh` do no argument handling at all, so
  handed `--list` they run their entire premerge suite and exit 0. The graph
  answers instead — only a Rust binary can carry libtest — and a Rust harness
  that *does* refuse `--list` must be declared in `NOT_LISTABLE` with a reason
  stating what its opacity hides. An undeclared refusal fails the gate, because
  the alternative is 902 tests silently collapsing into one opaque unit under a
  green check.
- **Allowances are declared with a reason** in the script's `ALLOWANCES`
  ledger. An entry is either `per-target` — a roster of targets that each
  repeat across the named platforms on their own — or `between-targets`, an
  exact set that overlaps. Nothing matches by subset in either direction, so a
  roster cannot vouch for a *new* overlap between two of its own members, and a
  two-target entry cannot absorb a duplication between two CLI shards.
  Rosters go stale per target, not per entry. Absence never implies permission.

Four duplication families are declared today. The native lanes' repetition of
the graph-owned platform unit targets, and their
`scripts/rue cli abi|cli.linker|cli.fs_file_io` steps re-running cases the CLI
shards run, are both the platform responsibility matrix doing its job. The
compiler split is now represented by its focused native target, so no broad
compiler cross-platform allowance remains. The release-smoke
overlap is deliberately retained only between the debug CLI shards and the
release-configured release job; `rue_ci_dedicated_lane` prevents a third debug
execution in linux-premerge. The broad native ABI filter carries an exact skip for
`cli.differential_opt::aggregate_abi_across_opt_levels`, so all other ABI
coverage remains and that case cannot return as an accidental substring match.

Cost, measured on a 4-core-class host with the binaries already built: **0.29s
wall for 73 `--list` invocations**, 1.6s of process time run concurrently. The
gate materializes only what a listing consults, which deliberately excludes
`RUE_CLI_STAGED_PROGRAMS`: `//examples:cli-staged-programs` stages ten `rue_program`
compiles that discovery never reads, the premerge lane does not otherwise build
it, and the shards that do consume it run on other runners. The step is skipped
on a narrowed pull-request run, where it would have to build unimpacted test
binaries and spend the wall time RUE-1130's narrowing exists to save;
duplication is a property of the graph rather than of the diff, and the
authoritative merge-group run always evaluates it.

### Where it cannot see

Every run prints an `opaque:` line counting the units whose contents the
comparison cannot reach, so a passing log says how much of the graph the
verdict covers. Three groups:

- **The manifest-driven corpora on the native lanes.** Those lanes register the
  `only_on` cases for their own host, which a linux-x64 gate cannot enumerate.
  The RUE-1161 platform responsibility gate covers that surface instead.
- **Harnesses that are not libtest.** `//:reproducible-programs`,
  `//:oracle-diff-generated-smoke`, `//:frontend-diff-test`, and
  `//:spec-traceability` each count as one unit.
- **The oracle differentials**, which are intentionally distinct assertions:
  `//crates/rue-oracle-diff:oracle-diff-test` and `:oracle-diff-spec-test` drive
  every runnable CLI and specification case through the reference interpreter
  and compare it against the compiler. They may execute overlapping corpus
  inputs, but the interpreter comparison is the assertion and is not repeated
  coverage. Their harness-owned runtime eligibility and argument grammar make a
  `--list` inventory non-authoritative, so they remain explicitly classified in
  `NOT_LISTABLE` and outside ordinary duplicate accounting.

## Affected-target selection on pull requests (RUE-1119)

On a `pull_request` run, the heavy `platform-corpus` suites and registered lanes
are selected down to the work the change actually affects; `merge_group` and
`workflow_dispatch` always run the complete live inventories and remain the
authoritative `//...` gate. Selection uses Meta's off-the-shelf Buck Target
Determinator (BTD, `facebookincubator/buck2-change-detector`) rather than a bespoke
`owner()`/`rdeps()` query: the `affected-targets` job dumps the Buck graph with
`buck2 targets` at the merge-base and at the head and feeds both dumps plus the
changed-file list to `btd`, whose impacted-target closure is intersected with
the platform-corpus set and each gated lane's targets. `btd` is a checked-in
DotSlash manifest for the
immutable 2026-07-20 release; its archive size, BLAKE3 digest, platform mapping,
and extraction path are reviewed in-tree before CI downloads it.

The selection is **conservative and fail-open** — under-selection silently
drops coverage (the RUE-924 failure mode), so every uncertain path runs the
whole corpus. `scripts/affected-targets` forces a full run whenever the diff
touches an out-of-graph or graph-global input — the `./buck2` pin, `test.sh`,
any `scripts/ci-*` runner, the Valgrind installer, the selection engine itself,
the workflow files, or
`.buckconfig`/`BUCK`/`*.bzl`/`toolchains`/`platforms`/`prelude`/`rust-toolchain.toml`
— and it falls back to full on any VCS, provisioning, `buck2`, `btd`, or output
parsing error.
Because the determinator job always exits with a decision (full on error), it
never blocks the merge queue, and a core-compiler change fans out through BTD's
reverse-dependency closure to the whole corpus exactly as before. The merge
queue is the only path to trunk and `merge_group` always runs full, so
under-selection on a pull request costs one queue ejection, never a merged
regression (RUE-1935); that is why the determinator is a thin wrapper around
BTD rather than a proof system. It publishes five outputs — `full`,
`selected` (corpus targets), `selected_lanes`, `narrowed`, and the multiline
`impacted` closure — plus the `corpus_matrix` the shard planner derives. The
one documented transport hazard is an *undeclared* job output, which GitHub
resolves to the empty string; `scripts/validate-ci-gate.py` fails closed on
any `needs.<job>.outputs.<name>` reference without a declaration (RUE-1130).
The deterministic force-full, gate, scope, and decision logic is pinned by
`scripts/test-affected-targets.sh` with fake git, BTD, and `buck2 targets`.

Selection is applied **within** each gated job, not by skipping the job:
`scripts/ci-corpus-selected` decides at job start and writes `run=true|false`;
`run=false` only when the decision was explicitly selective and the unit is
absent from its list (corpus targets read `selected`, lanes read
`selected_lanes`), so anything unset or malformed runs. A deselected unit skips
the heavy steps (paying only the runner spin-up) while the check still reports
success, so no branch-protection change is required. The validator also
requires every `ci-corpus-selected` step to name a lane the determinator can
select and to read the output that carries that kind of selection; a lane
name it never emits would otherwise be deselected on every selective run.

The platform-corpus set is derived from the graph (RUE-1936):
`scripts/affected-targets corpus-targets` is every cached corpus
(`_corpus_action`) that `scripts/ci-heavy-suite` runs (`rue_heavy_suite`) and
that required CI owns in a dedicated job — `rue_ci_dedicated_lane`, or a
`rue_cli_shard` slice — with a corpus whose shards carry the label represented
by its shards. The same output feeds `scripts/plan-cli-shards.py`, which
refuses an empty or non-exhaustive inventory, so a graph query failure fails
the planning job closed rather than shrinking the matrix. Dropping the label
from a corpus therefore removes it from the matrix, which is exactly the edit
the `ci-contract` job's live tier validator (`--live-graph`) and
`validate-ci-gate.py`'s dedicated-lane ownership check exist to catch.

The gate covers eight named lanes: `clippy`, `native (linux-arm64)`, `native
(macos-arm64)`, `release (linux-x64)`, `valgrind (linux-x64)`, `asan
(linux-x64)`, `compiler reproducibility (linux-x64)`, and `rue_program digest
sensitivity (linux-x64)`. Each is named in `SELECTABLE_LANES` and maps to the
Buck targets it actually executes (`lane_targets`); a lane runs when any of
those targets is in BTD's impacted closure. Gating the corpora alone had saved
nothing measurable: a documentation change cost 444s against 465s for a
compiler change, because the lanes that dominate a run were not consulting the
determinator. On four measured peripheral runs the RUE-1130 extension freed
905–1034s of runner time each.

Clippy is gated like every other lane and narrowed like the native units. Its
one canonical live inventory is every `sh_test` under `root//crates/...` whose
label ends exactly in `-clippy`; that same computation supplies both its
lane-selection proxies and its runnable scope. Every member also carries
`rue_ci_clippy_lane`, and the live CI validator requires the label-owned set to
equal that canonical inventory exactly before Linux premerge may subtract it.
`scripts/ci-clippy run` runs `impacted ∩ clippy` when the determinator
published a closure, and the full inventory otherwise; a verified empty
intersection is an intentional no-op, a failed live query falls open to
`//crates/...`, and a successful live query with zero clippy targets is a hard
error, because it means the query or the crate macros are broken. The count
and content-proof layer that once guarded these outputs was removed by
RUE-1935: it defended against a corruption GitHub does not produce, and only
this lane ever consulted it.

`linux-premerge` is handled differently, because skipping it wholesale on a
representative subset is exactly the RUE-924 failure mode. It is **narrowed**
instead of gated: the determinator publishes the impacted closure as the
`impacted` output, and the lane runs `impacted ∩ tier` in place of
`//... ∩ tier`, for both its build step and `test.sh`. Membership still comes
from the live graph, so a target added since any list was written is still
discovered — it is simply not built or run when the diff cannot reach it. That
is where the build cost goes: the lane spends 286–317s building every crate
whenever a compiler crate changes, and an unimpacted crate's test binary has
nothing to prove. Each narrowing consumer names its scope to
`scripts/affected-targets narrow-scope LANE FILE` — `linux-premerge-build`,
`linux-premerge-tests`, `native-platforms-units`, or `clippy` — and receives
exactly `scope ∩ impacted`. The build scope is `//crates/...` minus the
reverse-dependency closure of every corpus action a required lane owns, in
both its narrowed and unnarrowed spellings: building a `_corpus_action` runs
the corpus, and the unnarrowed premerge build once ran the oracle
differentials in full beside the lanes that own them (RUE-1511).

Narrowing is declined, and the ordinary scope used, whenever nothing is
impacted or so much is that the pattern is the better expression of it
(`RUE_AFFECTED_NARROW_LIMIT`, default 600 targets — a compiler change reaches
most of the graph, so narrowing it saves nothing). Consumers key on the
determinator's `narrowed` flag rather than on the list being non-empty. An
absent, malformed, corrupt, or oversized candidate therefore reads as "run
everything"; scope-query or intersection failure does too. `test.sh`
applies the same rule: an unset, unreadable, or empty `RUE_TEST_TARGETS_FILE`
runs the full pattern, and only a readable non-empty list narrows.

One consequence is worth stating plainly: an explicit target list that filters
down to no tests is a legitimate outcome, and buck2 exits 0 reporting
`NO TESTS RAN`. That is indistinguishable by exit status from a narrowing bug,
so the selected count is echoed in the log and the job summary rather than left
to be inferred from a silent green.

The ASan harness is a standalone Cargo project outside the Buck graph, so BTD
cannot see it; `crates/rue-runtime-asan/` therefore forces a full run rather
than being represented by a proxy target. The `affected-targets` job writes
its two-line decision to the job summary, and each deselected job logs its
own intentional-deselection line — so a legitimate selective skip is never
confused with a silently dropped suite (RUE-924). Because branch protection
consumes only `CI success`, later matrix reshaping or coarser job-level gating
can proceed without changing the protected context. Caching the base graph
dump keyed by trunk commit remains a possible follow-up.

Major Buck commands run through `scripts/ci-timed`, which preserves output and
the exact command exit status while appending wall time and aggregate
`Commands: (cached / remote / local)` counters to the GitHub job summary. CLI
shard summaries also show the number of measured cases next to wall time. Read
wall time together with hit count: a small number of invalidated ThinLTO actions
can dominate a release build even when its hit rate is above 90 percent.

Each summary also reports the action-cache hit rate and the summed duration of
the test processes themselves. That last figure is deliberately separate from
the cached/remote/local counters, which describe only how Buck *obtained* each
action: a corpus lane whose wall time is one harness process is a sharding
problem, and a lane whose wall time is uncached actions is a cache problem.
`docs/process/build-cache.md` explains the distinction.

Containers executed by the workflow must use a reviewed, human-readable
release tag and the immutable OCI index digest for that tag. The repository
gate `//:required-ci-container-pin-validation` rejects a moving `latest` image
reference, and the normal `./test.sh` run includes that gate. The same gate
covers the BuildBuddy worker image in `platforms/remote_cache.bzl`, which
required CI's remote-execution canary executes: there it requires an immutable
digest outright, since that image publishes no reviewable release tag.

## Updating actionlint

1. Find the latest stable release and review its notes and Dockerfile:

   ```bash
   gh release view --repo rhysd/actionlint
   gh api 'repos/rhysd/actionlint/contents/Dockerfile?ref=v<VERSION>' \
     --jq .content | base64 --decode
   ```

   In particular, confirm that the image still installs ShellCheck. actionlint
   can use the image's `/usr/local/bin/shellcheck` executable to check every
   `run:` block while it discovers every workflow under `.github/workflows/`.

2. Resolve the reviewed release tag to its multi-platform OCI index digest:

   ```bash
   docker buildx imagetools inspect docker.io/rhysd/actionlint:<VERSION>
   ```

   Update the image in `ci.yml` as
   `rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST>`. Keep both parts: the tag
   records what humans reviewed, while the digest fixes the bytes CI executes.

3. Verify that the pinned image contains ShellCheck, run actionlint exactly as
   CI does, then run the repository policy and its focused regression tests:

   ```bash
   docker run --rm --entrypoint /usr/local/bin/shellcheck \
     rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST> --version
   docker run --rm -v "$PWD:/repo:ro" -w /repo \
     rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST> \
     -color -shellcheck=/usr/local/bin/shellcheck
   ./buck2 test //:required-ci-container-pin-validation \
     //:required-ci-container-pin-tool-tests
   ```

Both container invocations must finish successfully. Together they verify that
the image contains ShellCheck and that actionlint checks the `run:` blocks with
that binary while linting all discovered workflows.
