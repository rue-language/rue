# Remote build cache + execution (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). Every CI run
and every isolated worktree rebuilds unchanged crates. We use **BuildBuddy** (free
tier) for a shared remote action cache and opt-in remote execution.

> **Status (RUE-316/RUE-320).** The remote platform, `$ORIGIN` toolchain-tree
> fix, and execution-platform-scoped linker are in place. Native/default builds
> keep `clang++`; the BuildBuddy execution configuration selects the ubiquitous
> `cc` driver through an exec-dep while Rust's bundled lld performs the real
> Linux link via `-fuse-ld=lld`. Full remote execution is supported as an
> explicit `--prefer-remote` mode, not the default local-development policy.

- **Remote action cache**: the repository `./buck2` wrapper supplies
  `--prefer-local`, so cache misses execute locally while hits are shared across
  machines + daemon restarts. The cache can only affect *speed*, never
  correctness.
- **`--prefer-remote`**: full remote **execution** — compiles + links on
  BuildBuddy's container (~80 free-tier cores). Required merge-group CI runs a
  cache-disabled canary so worker-toolchain regressions cannot hide as hits.

Tracking: RUE-316 (groundwork), RUE-320 (platform-scoped linker).

## Local development across worktrees

```bash
scripts/rue cache install       # prompts without echo for the BuildBuddy key
scripts/rue cache apply --all   # primary checkout + current Git/Codex worktrees
```

`install` writes one user-owned configuration at
`${XDG_CONFIG_HOME:-~/.config}/rue/buildbuddy.buckconfig`, with mode `0600`.
`apply` places only an ignored `.buckconfig.local` symlink in each checkout, so
the credential is neither copied between worktrees nor stored in Git. It refuses
to replace an existing local config and refuses a central config readable by
another account. Commands never print the key. A secure config installed before
the Rust upload gate existed is upgraded atomically in place, preserving its
credential; an explicit `default_allow_cache_upload = false` remains an error
instead of being silently overridden.

Once installed, direct `./buck2`, `scripts/rue ...`, and `test.sh` runs
automatically link a new worktree on first use. If the central config is absent,
Rue simply uses its ordinary local Buck configuration. If it is malformed or
insecure, Rue warns and continues without provisioning it. Existing local paths,
including broken or unrelated symlinks, are left untouched. This keeps cache
setup opt-in and prevents a credential problem from making local builds unusable.

## Host-wide disk lifecycle

Every worktree has its own `buck-out`; a large primary checkout and many smaller
worktrees therefore share one host budget even though Buck daemons cannot share
their local materializer state. Rue configures Buck's deferred materializer to
persist that state, defer write actions, and continuously remove outputs that
have not been used for one week. Cleanup starts twelve hours after daemon startup
and repeats daily. Buck coordinates those deletions with active builds; Rue does
not infer that a worktree is disposable from its age or directory name.

The background cleaner also watches the host filesystem. At 20% free space or
lower it adaptively promotes the oldest non-active outputs until Buck projects
that 20% will be free again. Outputs accessed in the last twelve hours remain
protected even under pressure. This threshold is host-wide—the worktrees have
separate materializer databases, but they consume the same filesystem budget.

Use the host-wide storage command from any current Rue checkout:

```bash
scripts/rue storage status            # sizes, source state, and cache state
scripts/rue storage plan [AGE]        # Buck dry-run in every registered worktree
scripts/rue storage clean [AGE]       # stale + adaptive cleanup; default 1w
scripts/rue storage guard             # run the build preflight explicitly
scripts/rue storage reset /exact/root # full Buck reset of an explicit target
```

Every `./buck2 build`, `test`, `run`, or `install` invocation runs the same
portable free-space preflight. Above 10% free it is only a `df` read. At 10% or
lower it synchronously requests adaptive cleanup from every registered Rue
worktree before allowing more disk-heavy work. If inventory or cleanup fails,
the command stops with recovery guidance instead of proceeding toward ENOSPC.
If tracked cleanup cannot escape the emergency threshold, the guard may fully
reset the largest legacy `buck-out` trees whose checked-out revisions predate
deferred materializer state. It uses the coordinator checkout's pinned Buck and
refuses to reset a root with an active command. Dirty source state is unrelated
and remains untouched. Between 10% and 20%, Buck's ordinary background policy
restores headroom without putting every build behind a host-wide scan.

The inventory comes from `git worktree list` and fails closed: if the registered
set cannot be read, no cleanup runs. `clean` removes stale or untracked Buck
outputs and may promote older tracked, non-active outputs when the host is below
the 20% target. `reset` is the migration escape hatch for an older worktree whose
artifacts predate persisted materializer state; it validates every named path as
a registered Rue worktree before it resets any of them. Neither command removes
source files or worktrees. `scripts/rue gc` remains a compatibility alias for
the host-wide one-week stale cleanup, and the older `scripts/worktree-gc` entry
point now performs the same safe cleanup without deleting directories.

The default setup is for the shared **action cache**. Normal commands stay on
`--prefer-local`; add `--prefer-remote` explicitly when remote execution is the
intended experiment. The checked-in `.buckconfig.local.example` remains a
reference for the generated configuration, not the recommended per-worktree
setup.

## Full-suite host coordination

`scripts/rue test` with no filter takes a user-scoped lock under `/tmp`, shared by
independent Rue project roots. Only one full suite runs on the host at once;
`scripts/rue quick`, filtered tests, and direct targeted Buck commands remain
available concurrently. A waiter reports the current holder once and then at
most once per minute. Owner PID metadata and atomic stale-lock recovery handle
normal exit, interruption, and a process that died while acquiring the lock.

Within the full suite, Buck first discovers and runs all non-heavy tests with
`//...`. The spec, UI, CLI, generated-oracle, and program-reproducibility targets
carry the `rue_heavy_suite` label. `test.sh` queries that label from Buck's live
target graph and runs every result one at a time, preserving independent cache
entries without a hand-maintained inventory that can omit a new suite.

## What it took (the non-obvious bits)

Getting from "no cache" to "full RE" hit several gaps; all are now handled, but
they're recorded here so a future config change doesn't silently regress:

1. **Connection knobs** (`.buckconfig.local`): the addresses need a **`grpc://`**
   scheme (buck2 rejects `grpcs://`; `tls = true` upgrades the transport), and
   **`[buck2] digest_algorithms = SHA256`** (BuildBuddy's CAS digest). Without
   these the RE client silently never connects (`remote: 0`, no error).
2. **`remote_enabled = True` plus `--prefer-local`**: OSS buck2 only opens the RE
   connection when remote is enabled — *even for cache-only use*. A pure
   `remote_enabled = False` cache config connects to nothing. Buck's limited
   hybrid mode otherwise prefers remote execution, so the repository `./buck2`
   wrapper adds `--prefer-local` to ordinary build/test/run/install commands.
   Remote cache lookup still happens before a local miss executes. An explicit
   execution-mode flag overrides the wrapper.
3. **Two cache-upload gates**: the execution platform's
   `allow_cache_uploads = True` permits local-result uploads, but ordinary Rust
   actions also defer to the OSS-default-off
   `[buck2] default_allow_cache_upload = true` setting. Buck 2026-07-01 still
   hard-disabled uploads for most Rust actions; Buck 2026-07-15 contains the
   upstream fix that makes them honor this setting. Both the pin and the config
   knob are required — without them the client connects and downloads CAS
   inputs, but locally compiled Rust results never populate the action cache.
4. **rustc under RE** (`toolchains/rust/defs.bzl`): rustc finds `librustc_driver.so`
   via its native `$ORIGIN/../lib` RPATH, which only resolves if the whole rustc
   component tree is materialized on the remote worker. The `compiler`/`rustdoc`
   RunInfo carry that component plus the separate standard-library component as
   hidden inputs, so RE uploads the compiler co-located and preserves the merged
   sysroot's relative links. This is the relocatable, canonical fix — *not* an
   absolute-path `LD_LIBRARY_PATH` hack. Clippy and rustfmt are separate official
   component archives; Rue does not materialize the monolithic distribution's
   unused Cargo, rust-analyzer, LLVM tools, or documentation payloads. On macOS
   ARM64 this reduces the unpacked host toolchain from about 1.67 GiB to 492 MiB
   (71 percent), and the archives are execution dependencies so debug and
   release target configurations share that payload.
5. **Container** (`platforms/remote_cache.bzl`): pinned to `rbe-ubuntu22-04`
   (Python 3.10 — the prelude's rustc wrapper needs ≥3.9; the default image ships
   3.6), by immutable digest rather than by its moving tag. See
   "Updating the remote worker image" below.
6. **Linker** (RUE-320): the remote worker needs **`cc`** instead of the absent
   `clang++`. A `remote-execution` constraint is inserted only into the explicit
   full-remote execution configuration. The cache-only configuration omits it
   because its misses execute natively under `--prefer-local`. The C++ tools
   provider is an exec-dep, so its linker select sees that constraint and
   chooses `cc`; native/default and cache-only execution configurations retain
   the prelude's `clang++`. The pinned prelude adds `-fuse-ld=lld` for Linux,
   keeping the actual linker hermetic.
7. **Rust action memory** (RUE-320): a cache-disabled cold graph exceeded
   BuildBuddy's default per-action memory estimate and the executor OOM-killed
   rustc. The remote platform requests 4 GB per action; this is an execution
   scheduling hint and does not affect native or cache-only builds.

Change 4 (`$ORIGIN` toolchain-tree) is global and local-safe. Changes 1, 2, 3,
5, 6, and 7 live in the opt-in `.buckconfig.local` / `remote_cache` execution
path.

## CI

CI reads the key from the `BUILDBUDDY_API_KEY` repo secret (never from a file).
The cache is provisioned (RUE-1006/RUE-1019) in the `CI` workflow's `clippy`,
`release`, ordinary platform-test, and macOS corpus jobs, and in the sanitizer
`valgrind` job, via `scripts/provision-build-cache install && apply` gated on
secret presence.

Availability rules, which the workflow steps must respect:

- **Fork `pull_request` runs have no secret.** GitHub withholds repository
  secrets from any workflow run whose head branch lives in a fork, regardless
  of the author's permissions — and the normal contribution flow here is
  fork-based. Those lanes build cold, exactly as before the cache existed. The
  provisioning step therefore treats an empty key as "skip", never "fail".
- **`merge_group` runs have the secret.** The merge queue is the serial
  bottleneck, so this is where the warm cache pays off. These runs execute
  already-approved, queued code, which is also why letting them write to the
  shared cache (`allow_cache_uploads`) is acceptable.
- **The dedicated compiler-reproducibility job is intentionally cache-free.**
  It runs `scripts/check-reproducible-compiler.sh` (RUE-617), which hard-errors
  on a `.buckconfig.local`: the reference and relocated candidate builds must be
  identically configured for the byte comparison to indict path/scheduling/
  environment leaks rather than configuration drift. Keeping that proof in an
  independent job lets the ordinary linux-x64 build and tests use the shared
  cache without changing the reproducibility contract.

The `cache-probe` workflow (`.github/workflows/cache-probe.yml`) remains the
measurement tool: it writes a transient config from the secret, does a cold
release build of `//crates/...` then a clean-and-rebuild, and reports buck2's
`Commands: (cached / remote / local)` line. Each workflow attempt injects a
unique, otherwise-unused Rust cfg so prior runs cannot satisfy the nominal cold
phase. The probe fails unless that phase executes local actions and the warm
phase increases cache hits while reducing local actions. It runs weekly
(Mondays 05:00 UTC) and on demand with `gh workflow run cache-probe.yml`.

The schedule exists because cache degradation is silent by construction: the
cache can only change speed, never correctness, so a dead or non-populating
cache turns no lane red. Without a recurring genuinely-cold-then-warm probe, the
regression surfaces only as CI gradually becoming expensive.

The same workflow carries `rue-program-warm`, ADR-0070 Phase 1's positive
warm-cache control (RUE-1405, `scripts/check-rue-program-warm-cache.sh`). It
cold-builds the two canary `rue_program` targets under the same nonce
mechanism, then runs their four consuming scenarios from a *relocated*
checkout root and fails unless every canary scan/derive/compile action was a
cache hit. Cross-root service is the property the derive step's manifest
re-anchoring exists to guarantee, and it is what lets a `pull_request` run's
compile be consumed by the `merge_group` run.

Required CI also records per-step wall time and aggregate cached/remote/local
command counts in each job summary via `scripts/ci-timed`. The probe answers
whether unchanged actions are reusable; the job summaries answer the different
question of how expensive a real change's remaining local actions were. Both
signals matter: a central Rust or ThinLTO change can have a high numerical hit
rate while a few invalidated actions remain on the critical path.

Each summary separates the four costs that a single wall-time number conflates:

| Column | Question it answers |
| --- | --- |
| Cached | how many actions the remote cache served |
| Remote | how many executed on the BuildBuddy worker |
| Local | how many executed on the runner |
| Test time | how long the test processes themselves ran |

The first three are Buck's own accounting of how each action was *obtained*.
The fourth is not: Rue's spec, UI, and CLI corpora are entire harnesses behind a
single `sh_test` action, so their runtime is opaque to those counters. A corpus
lane can report a 95 percent hit rate and still spend nearly all its wall time
in one harness process — that is a corpus-sharding problem, not a cache problem,
and the two are only distinguishable when the summary reports both.

The separation is not a presentational choice: the first three counters *cannot*
see test execution. A `buck2 test` run is handed to the test executor rather than
evaluated as an action, so it never enters the `Commands:` accounting, and OSS
buck2 ships no test-result cache to serve it from. In one `merge_group` run,
`//:cli-tests-caldera` (which executed nothing) and `//:cli-tests-shard-1` (which
ran for 11:18) both reported `Commands: 465 (cached: 465/464)`. A corpus job can
therefore print a perfect hit rate while re-running an eleven-minute suite in
full, and that reads as "everything was cached" to anyone who stops at the
cache columns. Test time is the signal that separates those two runs. RUE-1118
has the measurements.

RUE-320 also adds a merge-group-only remote-execution canary. It disables action
cache reads, selects the no-fallback `//platforms:remote_execution` executor, and
requires Buck to report remotely executed actions. This is deliberately
separate from the cache probe: one proves worker execution, the other proves
unchanged-action reuse.

## Corpus suites are build actions (RUE-1118)

Because test executions cannot be cached, a plain `sh_test` corpus re-ran on
every merge even when the merge commit's tree was byte-identical to the tree the
PR run had just validated. Three CI runs of one such tree — two `pull_request`,
one `merge_group` — converged to 465/465 cached build actions while every corpus
re-executed in full, at 91–97% of the merge queue's critical path.

`cached_corpus_suite` (`corpus.bzl`) therefore splits each heavy corpus in two:

- a **genrule** that runs the harness through `scripts/corpus-action` and writes
  a stamp on success. This is an ordinary action, so the PR run uploads it and
  the `merge_group` run reads it back like any compile;
- a thin **`sh_test`** that asserts the stamp. It keeps the suite's name, labels,
  and `Pass: root//:NAME (time)` result line, so `scripts/ci-heavy-suite` and
  test.sh's RUE-924 corpus-omission audit keep working unchanged.

Two consequences are worth knowing before changing a corpus target:

1. **The declared input contract is now load-bearing.** Under a plain `sh_test`
   an undeclared input was merely untracked — the suite re-ran regardless. Here
   an undeclared input is a false pass: change a file the action does not name
   and the corpus reports success against the previous tree's result. Every path
   a harness reads at runtime must reach it through `env`, and hence through
   `$(location ...)`.
2. **Paths must be absolute.** `$(location ...)` expands to an absolute path in
   an `sh_test`, which runs from the project root, but to a project-relative path
   in a build action. The harnesses spawn the compiler with each case's temp
   directory as cwd, and `find_dir` falls back to relative defaults, so a
   relative value can resolve to a real but wrong directory and quietly test
   nothing. `corpus-action` resolves everything named in the suite's
   `absolutize` list immediately before the harness runs.
3. **The suite must be a rule that permits cache uploads.** `cached_corpus_suite`
   defines its own rule rather than using `genrule` for one reason: the prelude
   computes `cacheable = attrs.cacheable and (local_only or prefer_local)` and
   passes that as `allow_cache_upload`, where those two flags come from a
   Meta-internal label allowlist (`uses_sudo`, `qt_moc`, `yarn_install`, ...). A
   plain genrule therefore never uploads here, and the first merge_group run of
   RUE-1118 re-executed every corpus on a tree byte-identical to the one the PR
   run had just built. The rule sets `allow_cache_upload = True` explicitly.
4. **A measurement the corpus produces must be a declared output, not a path
   handed in.** RUE-1158 rebalances the CLI shards from per-case timings, which
   `ci-heavy-suite` used to collect by passing `--env RUE_CLI_CASE_TIMINGS=` to
   the test executor. Neither half of that survives the conversion: the harness
   now runs inside the action, where an executor `--env` never reaches it, and
   the path was a per-run `mktemp`/`RUNNER_TEMP` value, which would change the
   action's digest on every run and defeat the caching entirely. The timings are
   a second declared output (`cached_corpus_suite(case_timings = True)`, exposed
   as `:NAME-action[timings]`), so they are stored with the stamp and
   materialize on a cache hit. `ci-heavy-suite` fetches that sub-target after the
   run and copies it to the path `ci.yml` uploads. The general rule: anything a
   corpus *produces* that outlives the run belongs in the action's outputs, or it
   silently stops existing the moment the suite starts being cache-served.

### rue_program compile actions (ADR-0070, RUE-1404)

`rue_program` (`rue_rules.bzl`) is the second cacheable writer class after the
corpus actions: its scan (`rue --emit deps`), manifest derivation, and compile
all set `allow_cache_upload`, so a PR run's program compiles are served to the
merge-group run like any rustc action. Three properties matter for cache
reasoning:

- **The scan's output is machine-unstable but its key is not.** The dependency
  envelope embeds absolute paths and inode/mtime identity, and a cache-served
  envelope from another checkout is expected; the derivation step re-anchors
  through the envelope's recorded roots and emits manifest entries relative to
  the manifest's own directory, so the derived manifest is byte-identical
  across checkout roots. Consequently a scan/derive cache miss on one machine
  still converges to a compile cache HIT, because the compile is keyed on the
  manifest's content, not the envelope's.
- **The declared boundary is enforced at derivation, not by the cache.** An
  accepted read outside `srcs ∪ std` fails the build in-band; see
  `scripts/rue-program-derive-manifest.py` for why that check must not be
  simplified away.
- **`scripts/check-rue-program-digests.sh` is the standing control** (the
  `rue-program-digests` CI lane): declared mutations re-run the chain,
  undeclared neighbours do not, asserted as steady-state convergence because
  OSS buck2 has no persistent digest-keyed local action cache and
  "revert-is-a-cache-hit" is not a property it promises.

### Reading whether a corpus was cache-served

The thin `sh_test` reports `Pass: root//:NAME (0.0s)` whether the corpus ran for
eleven minutes or was served from cache — it only checks the stamp. **Do not read
that line as evidence of anything.** Two signals actually answer the question:

- `Commands: N (cached: C, remote: R, local: L)` — the corpus is one action, so a
  cache-served suite adds to `cached` and a re-executed one adds to `local`. The
  pre-RUE-1118 baseline for a CLI shard was 465 commands; it is 466 now. A
  shard's `cli-tests: measured N cases` line answers it too: the timings are an
  output of that same action, so `N` is the case count of whichever run produced
  the cache entry, not proof that this run executed anything.
- the job's wall time, which is unambiguous.

This matters because the two failure modes look identical in the test output. A
merge_group run that reports `Pass (0.0s)` on all eighteen corpus jobs while each
one takes ten minutes is doing no caching at all.

`//:reproducible-programs` and `//:oracle-diff-generated-smoke` remain plain
`sh_test`s: their harnesses are shell scripts that read repository paths
directly rather than through declared env inputs, so establishing that contract
has to come first. Both are off the merge queue's critical path.

## Updating the remote worker image

The merge-group remote-execution canary claims "the compiler builds on the
worker we reviewed". A moving tag would silently change what that proves, and
would convert an upstream image republish into a merge-queue failure with no
local reproduction. `//:required-ci-container-pin-validation` therefore rejects
a `platforms/remote_cache.bzl` image reference without an `@sha256:` digest, and
rejects a `latest` tag anywhere in required CI. BuildBuddy publishes no
versioned tag for this image, so unlike the actionlint pin the digest stands
alone rather than accompanying a reviewed release tag.

1. Resolve the moving stream to its current immutable OCI index digest:

   ```bash
   docker buildx imagetools inspect gcr.io/flame-public/rbe-ubuntu22-04:latest
   ```

   The index digest covers every architecture, so the executor still selects the
   linux/amd64 manifest from it.

2. Update `container-image` in `platforms/remote_cache.bzl` to
   `docker://gcr.io/flame-public/rbe-ubuntu22-04@sha256:<INDEX-DIGEST>` and
   refresh the resolution date in the comment above it.

3. Confirm the new image still satisfies the constraints recorded above —
   Python ≥3.9 for the prelude's rustc wrapper, and a `cc` driver for the
   RUE-320 linker select:

   ```bash
   docker run --rm gcr.io/flame-public/rbe-ubuntu22-04@sha256:<INDEX-DIGEST> \
     bash -c 'python3 --version && cc --version'
   ```

4. Run the policy and its focused regression tests, then let the merge-group
   `remote execution (linux-x64)` canary prove the worker end to end:

   ```bash
   ./buck2 test //:required-ci-container-pin-validation \
     //:required-ci-container-pin-tool-tests
   ```

## Toward a fully hermetic linker

`linker = "cc"` still depends on the container having *a* C driver. The fully
hermetic version — mirroring the rust toolchain — would ship clang/lld as its own
toolchain so RE has zero container dependency. Not needed for correctness (lld
already does the linking); a future hardening if we make RE the default.
