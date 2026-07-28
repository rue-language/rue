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
`Benchmarks` keeps its push trigger because per-commit measurement on trunk is
its purpose. Valgrind and ASan are part of this same CI dependency graph, so
they cannot complete outside the aggregate gate. Manual CI dispatch retains
the `large_program` selector for expanded Valgrind coverage.

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

## Platform responsibility matrix

| Owner | Required responsibility |
| --- | --- |
| Linux x86-64 | Complete target-independent premerge suite, all CLI shards, specification corpus, scaling matrix, release smoke, reproducibility, lint/metadata gates, Valgrind, and ASan |
| Linux ARM64 | Native compiler/linker build; compiler, codegen, linker, runtime/archive, runtime ABI, allocator, and target unit tests; every applicable `only_on` spec/CLI case; real ABI, linker, and filesystem CLI programs |
| macOS ARM64 | The same native responsibilities on Mach-O/macOS, including host-conditional compiler/archive tests, every applicable `only_on` case, native linker/runtime execution, and output publication |
| Linux cross-backend step | Explicit host-independent x86-64 and AArch64 compilation/encoding unit coverage |

Linux ARM64 and macOS ARM64 deliberately do not repeat the broad unit suite or
the specification corpus. Backend encoding logic is still tested explicitly
for both architectures on Linux, while native ARM64 lanes prove the host ABI,
object/linker path, runtime archive, syscalls, and platform behavior that
cross-compilation cannot.

The native lanes set `RUE_PLATFORM_CASE_SELECTION=native` through
`scripts/run-native-platform-corpus.sh`. Both manifest-driven harnesses then
register every case whose nonempty `only_on` list includes the current host;
unscoped target-independent cases and automatic examples are not registered.
This makes new platform-scoped cases self-enrolling. The native lanes also
retain the explicit ABI, linker, and filesystem CLI filters because those
suites contain native-execution cases with empty `only_on` lists. The CI
contract pins both parts of this union, along with the host-conditional
`rue-compiler-test` unit target.

The build jobs use the shared BuildBuddy remote action cache when the
`BUILDBUDDY_API_KEY` secret is available (merge_group runs; fork PRs build
cold) — see `docs/process/build-cache.md` for the availability rules. The
compiler-reproducibility job is the deliberate exception: its two independently
materialized compiler builds stay local and cache-free, while the ordinary
linux-x64 test lane may use BuildBuddy.

Required release coverage is intentionally focused (RUE-1129). The
`release (linux-x64)` job analyzes Buck's configured `rustc_cfg` actions and
fails unless `//platforms:release` supplies `-Copt-level=3 -Clto=thin` while
`//platforms:debug` supplies neither. It then runs `//:release-smoke`, the 24
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

Production `debug_assert*` use is governed by
`scripts/validate-debug-assert-policy.py`, run by required formatting CI and
`scripts/rue quick`. Every surviving call has an exact per-file allowance and
rationale; changing the count requires reviewing the ledger. CFG optimization,
code generation, and linking have no allowances: an invariant whose violation
could change emitted code must be an always-on assertion or a real compiler
diagnostic.

## Test execution tiers

Every first-party Buck test target carries exactly one execution-tier label:
`rue_test_tier_premerge`, `rue_test_tier_slow`, or
`rue_test_tier_stress`. The wrappers in `test_defs.bzl` attach those labels to
repository, crate, and toolchain tests; generated crate unit tests default to
`premerge`. A test that belongs outside pre-merge must opt into `slow` or
`stress` at its definition rather than returning success from a skipped body.
The required Valgrind and ASan jobs are logical `premerge` coverage and remain
explicit jobs inside the CI aggregate: Valgrind provisions an external runtime
tool, while ASan requires a pinned nightly Cargo toolchain that Buck does not
model.

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

The exhaustive CLI- and specification-oracle differential harnesses are
`slow`: premerge retains the fixed generated oracle smoke corpus as its bounded
codegen canary. CLI corpus sections may likewise declare `tier = "slow"`.
Automatic examples have the same declarative tier field.
`//:cli-tests-slow` owns those real cases. Mosaic's shipped-program smoke and
17 exhaustive behavior cases are slow, so the four required CLI shards do not
spend their critical path compiling that large program. Caldera and Meridian
instead use dedicated reduced premerge roots plus compile-once slow targets.

The normal 100/1k structural scaling matrix remains a dedicated premerge
canary; `//crates/rue-compiler:scaling-matrix-stress-test` enables the real
10k-per-axis ladder and belongs to `stress`. Caldera and Meridian likewise
keep their 4x configurations in dedicated stress targets.

Execution tier and scheduling are separate concerns. A required pre-merge test
may also carry `rue_heavy_suite` or `rue_dedicated_suite` so it runs in an
isolated lane without being misrepresented as scheduled-only coverage.
`scripts/rue quick` excludes non-unit integration harnesses via
`rue_not_quick` and dedicated suites such as the structural scaling matrix.
The full local suite still discovers and runs them once. Required CI
sets `RUE_CI_DEFER_DEDICATED_SUITES` to the exact live set of
`rue_dedicated_suite` targets, so the platform broad passes exclude work owned
by explicit parallel jobs. `test.sh` fails unless the environment and Buck
graph match exactly; a new dedicated target therefore cannot be silently
dropped. The scaling matrix now runs only in its dedicated Linux job instead
of once in each platform broad pass and then again in that job.

The Linux premerge lane retains broad target discovery and defers its two
heaviest corpora — `//:cli-tests` and `//:spec-tests` — to explicit Linux
`platform-corpus` jobs so those corpora overlap the main lane instead of
serializing behind it (RUE-1115). `test.sh` accepts that deferral only under
`CI=true`, validates each target against Buck's live `rue_heavy_suite` query,
and continues to audit every corpus target it owns. Local full suites never
defer coverage. Native ARM64 lanes use the explicit responsibility matrix
above instead of broad discovery.

Caldera and Meridian are absent from the ordinary CLI corpus by explicit
filters because their complete generated graphs are slow-tier workloads. The
required broad pass instead runs `//:large-example-caldera-canary` and
`//:large-example-meridian-canary`, reduced roots that exercise each
application's core compiler/runtime path without claiming full-program
coverage. Nightly `//:large-example-{caldera,meridian}-slow` targets compile
and execute the real roots with no success-stub environment. Their `stress4`
configurations live only in the corresponding `-stress` targets.

Required Valgrind coverage explicitly sets
`RUE_SANITIZER_LARGE_PROGRAMS=none`; it does not quietly recurse around one
large directory while including the other. Manual sanitizer dispatch can
select `caldera`, `meridian`, or `all`. The ASan job instruments the Rust arena
allocator rather than compiled Rue applications, so that selection does not
apply to it.

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
heavy-suite discovery). Nothing else re-runs the slices on required CI, so
`//:cli-shard-coverage-validation` fails the build if the shard targets in
`BUCK` and the `platform-corpus` matrix in `ci.yml` ever drift apart. Changing
`CLI_TEST_SHARD_COUNT` therefore means updating the matrix; the protected
`CI success` context remains unchanged.

### Correctness hang guards and performance budgets

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

Performance thresholds live in `benchmarks/manifest.toml` and are enforced by
the separate Benchmarks workflow. A correctness timeout therefore never
asserts that a case is fast enough. Conversely, benchmark regressions are
reported as performance failures rather than being recast as correctness
timeouts.

The weekly Correctness repetitions workflow runs every ordinary CLI shard
multiple independent times. It uploads per-run logs and a summary, continues
after a failure only to gather flake evidence, and exits failed if *any*
repetition failed; a later pass never masks an earlier failure. Required
correctness jobs do not automatically retry failed cases.

## Affected-corpus selection on pull requests (RUE-1119)

On a `pull_request` run, the heavy `platform-corpus` suites are selected down to
the ones the change actually affects; `merge_group` and `workflow_dispatch`
always run the full corpus and remain the authoritative `//...` gate. Selection
uses Meta's off-the-shelf Buck Target Determinator (BTD,
`facebookincubator/buck2-change-detector`) rather than a bespoke
`owner()`/`rdeps()` query: the `affected-targets` job dumps the Buck graph with
`buck2 targets` at the merge-base and at the head and feeds both dumps plus the
changed-file list to `btd`, whose impacted-target closure is intersected with
the selectable corpus set. `btd` is a checked-in DotSlash manifest for the
immutable 2026-07-20 release; its archive size, BLAKE3 digest, platform mapping,
and extraction path are reviewed in-tree before CI downloads it.

The selection is **conservative and fail-open** — under-selection silently
drops coverage (the RUE-924 failure mode), so every uncertain path runs the
whole corpus. `scripts/affected-targets` forces a full run whenever the diff
touches an out-of-graph or graph-global input — the `./buck2` pin, `test.sh`,
any `scripts/ci-*` runner, the selection engine itself, the workflow files, or
`.buckconfig`/`BUCK`/`*.bzl`/`toolchains`/`platforms`/`prelude`/`rust-toolchain.toml`
— and it falls back to full on any VCS, provisioning, `buck2`, `btd`, or output
parsing error.
Because the determinator job always exits with a decision (full on error), it
never blocks the merge queue, and a core-compiler change fans out through BTD's
reverse-dependency closure to the whole corpus exactly as before. The
deterministic force-full and gate logic is pinned by
`scripts/test-affected-targets.sh`.

Selection is applied **within** each `platform-corpus` job, not by skipping the
job: `scripts/ci-corpus-selected` decides at job start, and a deselected corpus
skips the heavy steps (paying only the runner spin-up) while the check still
reports success, so no branch-protection change is required. The
`affected-targets` job writes a selection manifest to the job summary accounting
for every corpus as `RUN` or `DESELECTED (intentional)`, and each deselected job
logs its own intentional-deselection line — so a legitimate selective skip is
never confused with a silently dropped suite (RUE-924). The selectable set in
`scripts/affected-targets` is the matrix gate's source of truth; an unknown
target fails safe toward running. Because branch protection now consumes only
`CI success`, later matrix reshaping or coarser job-level gating can proceed
without changing the protected context. Caching the base graph dump keyed by
trunk commit remains a possible follow-up.

Major Buck commands run through `scripts/ci-timed`, which preserves output and
the exact command exit status while appending wall time and aggregate
`Commands: (cached / remote / local)` counters to the GitHub job summary. CLI
shard summaries also show the number of measured cases next to wall time. Read
wall time together with hit count: a small number of invalidated ThinLTO actions
can dominate a release build even when its hit rate is above 90 percent.

Containers executed by the workflow must use a reviewed, human-readable
release tag and the immutable OCI index digest for that tag. The repository
gate `//:required-ci-container-pin-validation` rejects a moving `latest` image
reference, and the normal `./test.sh` run includes that gate.

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
