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
another account. Commands never print the key.

Once installed, direct `./buck2`, `scripts/rue ...`, and `test.sh` runs
automatically link a new worktree on first use. If the central config is absent,
Rue simply uses its ordinary local Buck configuration. If it is malformed or
insecure, Rue warns and continues without provisioning it. Existing local paths,
including broken or unrelated symlinks, are left untouched. This keeps cache
setup opt-in and prevents a credential problem from making local builds unusable.

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
   via its native `$ORIGIN/../lib` RPATH, which only resolves if the whole toolchain
   tree is materialized on the remote worker. The `compiler`/`rustdoc` RunInfo carry
   the full distribution as a hidden input so RE uploads it co-located. This is the
   relocatable, canonical fix — *not* an absolute-path `LD_LIBRARY_PATH` hack.
5. **Container** (`platforms/remote_cache.bzl`): pinned to `rbe-ubuntu22-04`
   (Python 3.10 — the prelude's rustc wrapper needs ≥3.9; the default image ships
   3.6).
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
phase increases cache hits while reducing local actions. Run it with
`gh workflow run cache-probe.yml`.

Required CI also records per-step wall time and aggregate cached/remote/local
command counts in each job summary via `scripts/ci-timed`. The probe answers
whether unchanged actions are reusable; the job summaries answer the different
question of how expensive a real change's remaining local actions were. Both
signals matter: a central Rust or ThinLTO change can have a high numerical hit
rate while a few invalidated actions remain on the critical path.

**Those counters cannot see test time, and reading them as if they can is the
trap this section exists to prevent.** Buck's `Commands: N (cached: C, remote: R,
local: L)` line counts *actions*. A `buck2 test` invocation also performs a test
execution, which is not an action: it is handed to the test executor, it never
appears in that line, and OSS buck2 ships no test-result cache. Measured in one
`merge_group` run, a corpus whose test took `0.0s` and a corpus whose test took
`11:18` both reported `Commands: 465`. A job can therefore print
`Cache hits: 100%` while re-running an eleven-minute suite, which reads as
"everything was cached" and is not.

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
   an `sh_test`, which runs from the project root, but to a path relative to the
   action's working directory in a genrule. The harnesses spawn the compiler with
   each case's temp directory as cwd, and `find_dir` falls back to relative
   defaults, so a relative value can resolve to a real but wrong directory and
   quietly test nothing. `corpus-action` resolves everything named in the suite's
   `absolutize` list immediately before the harness runs; it cannot be done in
   BUCK because a genrule cmd has no shell command substitution — `$(...)` is
   Buck's own macro syntax.

`//:reproducible-programs` and `//:oracle-diff-generated-smoke` remain plain
`sh_test`s: their harnesses are shell scripts that read repository paths
directly rather than through declared env inputs, so establishing that contract
has to come first. Both are off the merge queue's critical path.

RUE-320 also adds a merge-group-only remote-execution canary. It disables action
cache reads, selects the no-fallback `//platforms:remote_execution` executor, and
requires Buck to report remotely executed actions. This is deliberately
separate from the cache probe: one proves worker execution, the other proves
unchanged-action reuse.

## Toward a fully hermetic linker

`linker = "cc"` still depends on the container having *a* C driver. The fully
hermetic version — mirroring the rust toolchain — would ship clang/lld as its own
toolchain so RE has zero container dependency. Not needed for correctness (lld
already does the linking); a future hardening if we make RE the default.
