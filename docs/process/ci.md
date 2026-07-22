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
`BUCK`) hash-partitioned shards, `//:cli-tests-shard-0 .. -N`, so its ~12-minute
wall clock collapses to N parallel slices per platform (RUE-1116). Each shard
sets `RUE_CLI_TEST_SHARD=k/N`; the harness selects a stable 1/N slice of the
corpus by a fixed hash of the test name, and the shards' union is the full
corpus. The shards carry both `rue_heavy_suite` (so `ci-heavy-suite` runs them
and the broad pass skips them) and `rue_cli_shard` (so a local `./test.sh` full
run executes the monolithic `//:cli-tests` once instead of every slice —
`test.sh` subtracts the `rue_cli_shard` set from its heavy-suite discovery).
Nothing else re-runs the slices on CI, so `//:cli-shard-coverage-validation`
fails the build if the shard targets in `BUCK` and the `platform-corpus` matrix
in `ci.yml` ever drift apart. Changing `CLI_TEST_SHARD_COUNT` therefore means
updating the matrix (and branch protection) to match.

## Affected-corpus selection on pull requests (RUE-1119)

On a `pull_request` run, the heavy `platform-corpus` suites are selected down to
the ones the change actually affects; `merge_group` and `workflow_dispatch`
always run the full corpus and remain the authoritative `//...` gate. Selection
uses Meta's off-the-shelf Buck Target Determinator (BTD,
`facebookincubator/buck2-change-detector`) rather than a bespoke
`owner()`/`rdeps()` query: the `affected-targets` job dumps the Buck graph with
`buck2 targets` at the merge-base and at the head and feeds both dumps plus the
changed-file list to `btd`, whose impacted-target closure is intersected with
the selectable corpus set. `btd` is pinned and provisioned by
`scripts/install-btd` (immutable dated release, verified against the release's
shipped `.sha256`).

The selection is **conservative and fail-open** — under-selection silently
drops coverage (the RUE-924 failure mode), so every uncertain path runs the
whole corpus. `scripts/affected-targets` forces a full run whenever the diff
touches an out-of-graph or graph-global input — the `./buck2` pin, `test.sh`,
any `scripts/ci-*` runner, the selection engine itself, the workflow files, or
`.buckconfig`/`BUCK`/`*.bzl`/`toolchains`/`platforms`/`prelude`/`rust-toolchain.toml`
— and it falls back to full on any VCS, provisioning, `buck2`, or `btd` error.
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
`scripts/affected-targets` must stay in sync with the `platform-corpus` matrix;
an unknown target fails safe toward running. Coarser job-level gating (skipping
the runner entirely) and caching the base graph dump keyed by trunk commit are
possible follow-ups once a single `ci-success` aggregate check exists.

Major Buck commands run through `scripts/ci-timed`, which preserves output and
the exact command exit status while appending wall time and aggregate
`Commands: (cached / remote / local)` counters to the GitHub job summary. Read
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
