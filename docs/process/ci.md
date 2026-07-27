# Required CI

The `CI` workflow (`.github/workflows/ci.yml`) supplies the required pull-request
and merge-group checks.

## Triggers

`CI` and `Sanitizer` run on `pull_request` and `merge_group` only (plus
`workflow_dispatch` for manual re-validation). There is deliberately no
`push: [trunk]` trigger (RUE-1006): trunk only advances through the merge
queue, and the merge_group run's checks are attached to the exact commit that
lands, so a post-merge trunk run would re-test an identical tree. Do not
re-add a push trigger to these workflows; `Benchmarks` keeps its push trigger
because per-commit measurement on trunk is its purpose.

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

`.github/workflows/release.yml` runs the complete release-configured `//...`
suite nightly and on manual dispatch. Its `rue_cli_shard` exclusion is
intentional: `//:cli-tests` owns the shards' complete premerge union, while
`//:cli-tests-slow` owns declarative slow sections. Also running the four
CI-only shards would execute the premerge inventory twice.

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
The required Valgrind and ASan jobs are logical `premerge` coverage but remain
explicit workflow jobs: Valgrind provisions an external runtime tool, while
ASan requires a pinned nightly Cargo toolchain that Buck does not model.

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
Required CI sets `RUE_TEST_TIER=premerge`, while `scripts/rue all` and the
scheduled full-release workflow exercise the complete union.

The exhaustive CLI- and specification-oracle differential harnesses are
`slow`: premerge retains the fixed generated oracle smoke corpus as its bounded
codegen canary. CLI corpus sections may likewise declare `tier = "slow"`.
`//:cli-tests-slow` owns those real cases, while each maintained large
program's automatic example remains a single premerge compile/run canary.
Mosaic's 17 exhaustive behavior cases use that split, so the four required CLI
shards do not each spend their critical path recompiling the same large
program.

The normal 100/1k structural scaling matrix remains a dedicated premerge
canary; `//crates/rue-compiler:scaling-matrix-stress-test` enables the real
10k-per-axis ladder and belongs to `stress`. Caldera and Meridian remain
explicitly tracked by RUE-1162 until their real release-built slow targets and
reduced premerge canaries replace the current skips/stub.

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

The platform test lanes all retain broad target discovery. Every platform
(linux-x64, linux-arm64, macOS) defers its three heaviest corpora —
`//:cli-tests`, `//:cli-tests-caldera`, and `//:spec-tests` — to explicit
`platform-corpus` jobs so those corpora overlap the main lane instead of
serializing behind it (RUE-1115). Each architecture therefore has a matching
`cli`, `cli-caldera`, and `spec` shard in the `platform-corpus` matrix, and
those checks must be marked required in branch protection (a maintainer
action). `test.sh` accepts that deferral only under `CI=true`, validates each
target against Buck's live `rue_heavy_suite` query, and continues to audit
every corpus target it owns. Local full suites never defer coverage.

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
for newly discovered cases. The shards carry both `rue_heavy_suite` (so
`ci-heavy-suite` runs them and the broad pass skips them) and `rue_cli_shard`
(so a local `./test.sh` full run executes the monolithic `//:cli-tests` once
instead of every slice — `test.sh` subtracts the `rue_cli_shard` set from its
heavy-suite discovery). Nothing else re-runs the slices on CI, so
`//:cli-shard-coverage-validation` fails the build if the shard targets in
`BUCK` and the `platform-corpus` matrix in `ci.yml` ever drift apart. Changing
`CLI_TEST_SHARD_COUNT` therefore means updating the matrix (and branch
protection) to match.

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
target fails safe toward running. Coarser job-level gating (skipping
the runner entirely) and caching the base graph dump keyed by trunk commit are
possible follow-ups once a single `ci-success` aggregate check exists.

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
