# Remote build cache + execution (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). Every CI run
and every isolated worktree rebuilds unchanged crates. We use **BuildBuddy** (free
tier) for a shared remote action cache and opt-in remote execution.

> **Status (RUE-316/RUE-320/RUE-1937).** The remote platform, `$ORIGIN`
> toolchain-tree fix, and execution-platform-scoped C tools are in place. On
> Linux every compile and link runs through the hermetic Zig distribution's
> `zig cc`, selected through an exec-dep, so native, cache-only, and
> remote-execution configurations use the same linker bytes; macOS keeps the
> prelude's path clang tools. Full remote execution is supported as an explicit
> `--prefer-remote` mode, not the default local-development policy.

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
```

`install` atomically writes one user-owned configuration at
`${XDG_CONFIG_HOME:-~/.config}/rue/buildbuddy.buckconfig` with mode `0600`;
re-run it to replace the file, and it never prints the key. That is the only
step: the repository `./buck2` wrapper links `.buckconfig.local` (gitignored)
to that file in any worktree on the first `build`, `test`, `run`, or `install`
there, so the credential is never copied between worktrees or stored in Git.
The link, rather than a per-command `--config-file`, is the delivery mechanism
because `[buck2] digest_algorithms` and `[buck2_re_client]` are daemon-startup
settings: a `--config-file` leaves the daemon on SHA1 digests (which
BuildBuddy's CAS rejects) with no RE engine address, while a changed
`.buckconfig.local` restarts the daemon with both applied.

If the file is absent, Rue uses its ordinary local Buck configuration. An
existing `.buckconfig.local`, symlink or file, is never replaced, and a config
readable by another account is refused with a warning rather than linked;
neither ever blocks a local build. `RUE_NO_REMOTE_CACHE=1` skips the link for
one command, which is how the deliberately cache-free
`scripts/check-reproducible-compiler.sh` declines it (moving an existing
wrapper link aside for the duration), and `RUE_BUILDBUDDY_CONFIG` names a
different path for both `install` and the wrapper. A hand-written
`.buckconfig.local` (see `.buckconfig.local.example`) is left alone and still
makes the wrapper default to `--prefer-local`.

## Host-wide disk lifecycle

Every worktree has its own `buck-out`; a large primary checkout and many smaller
worktrees therefore share one host budget even though Buck daemons cannot share
their local materializer state. Rue configures Buck's deferred materializer to
persist that state, defer write actions, and remove outputs that have not been
used for one week; cleanup starts twelve hours after daemon startup and repeats
daily, and Buck coordinates the deletions with active builds. That policy is
what keeps a long-lived checkout bounded. It cannot help a worker worktree: a
short-lived daemon never reaches the twelve-hour offset, and output from work
still in flight is in use and must stay.

Per-worktree output is therefore reclaimed by a person, per worktree: remove a
worktree whose work is finished (`git worktree remove <path>`), or reset one
that stays. From any current Rue checkout:

```bash
scripts/rue storage status            # sizes, source state, and cache state
scripts/rue storage plan [AGE]        # Buck dry-run in every registered worktree
scripts/rue storage clean [AGE]       # Buck's tracked stale cleanup; default 1w
scripts/rue storage reset /exact/root # full Buck reset of an explicit target
```

The inventory comes from `git worktree list` and fails closed: if the
registered set cannot be read, no cleanup runs. `plan` and `clean` are Buck's
own `clean --stale AGE --tracked-only` in every registered worktree and nothing
more. `reset` validates every named path as a registered Rue worktree before it
resets any of them. Neither command removes source files or worktrees, and
`scripts/rue gc` remains a compatibility alias for the one-week `clean`.

Every `./buck2 build`, `test`, `run`, or `install` invocation reads free space
once (`df -Pk`) and refuses to start below 4 GiB, naming the remedies above; an
incremental build fits in that headroom, and a cold full build that does not
fails loudly with ENOSPC rather than corrupting anything. Above the floor the
wrapper does nothing, and no build ever cleans another worktree's output on its
own behalf: the earlier guard's cross-worktree cleanup caused RUE-1331 and
RUE-1683 and did not prevent RUE-1790 (123 MB free), because an age-based
policy cannot reclaim output the same cycle is still producing.

The default setup is for the shared **action cache**. Normal commands stay on
`--prefer-local`; add `--prefer-remote` explicitly when remote execution is the
intended experiment. The checked-in `.buckconfig.local.example` documents the
same knobs for a hand-written per-checkout config; the installed user config
is the recommended setup.

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
   "Updating the remote worker image" below. This number is the worker image's,
   for that wrapper, and is not the repository's Python floor — that is 3.9,
   in AGENTS.md under "Repository tooling baseline". The two are independent:
   the worker runs Buck actions, and the `remote execution (linux-x64)` job
   builds rather than tests, so repository `scripts/*.py` do not execute there.
   That the image's 3.10 happens to meet the 3.9 floor is incidental, not
   load-bearing.
6. **Linker** (RUE-320, then RUE-1937): the prelude's Rust rules take their
   linker from the C++ toolchain, and the worker image has no `clang++`. RUE-320
   answered that with a `remote-execution` constraint, inserted only into the
   explicit full-remote execution configuration, on which the exec-dep C tools
   provider selected the ubiquitous `cc`; the host compiler and its glibc still
   differed between the worker and a developer machine, so a link's bytes did.
   RUE-1937 replaced the host tools on Linux with the SHA-pinned Zig
   distribution already fetched for mimalloc: `toolchains//:zig-cxx-tools`
   wraps `zig cc`, `zig c++`, and `zig ar` (target `x86_64-linux-gnu.2.17` /
   `aarch64-linux-gnu.2.17`, selected on the execution platform's CPU) with
   the whole Zig tree as a hidden input and Zig's caches under
   `BUCK_SCRATCH_PATH`. The linker, its bundled lld, compiler_rt, libunwind,
   and the glibc symbol versions are then action inputs rather than host
   properties, and a Linux link is identical natively, from the cache, and on
   the worker. The `cc` override is gone; the `remote-execution` constraint
   remains on the platform for any future worker-only selection. The prelude
   still appends `-fuse-ld=lld` to Linux links; `zig cc` reports it as an
   unused argument and links with its own lld regardless. One detail keeps
   the compiler byte-reproducible across checkouts: Zig compiles the glibc
   start files, libunwind, and compiler_rt from source during the link, with
   DWARF whose `DW_AT_comp_dir` is the action's working directory, and any
   Zig strip request becomes LLD's `-s` (symbol table included). The linker
   wrapper therefore applies `toolchains/zig/runtime-debug-discard.ld`, which
   discards the debug sections of inputs under the Zig cache directories and
   nothing else. macOS is unchanged: the prelude's path clang tools plus
   `-ld_classic`.
7. **Rust action memory** (RUE-320): a cache-disabled cold graph exceeded
   BuildBuddy's default per-action memory estimate and the executor OOM-killed
   rustc. The remote platform requests 4 GB per action; this is an execution
   scheduling hint and does not affect native or cache-only builds.

Changes 4 (`$ORIGIN` toolchain-tree) and 6 (Zig C tools on Linux) are global
and local-safe. Changes 1, 2, 3, 5, and 7 live in the opt-in
`.buckconfig.local` / `remote_cache` execution path.

## CI

CI reads the key from the `BUILDBUDDY_API_KEY` repo secret (never from a file).
The cache is provisioned (RUE-1006/RUE-1019) via
`scripts/provision-build-cache install`, gated on secret presence, in
every `CI` job whose cost is a build — ten of them as of RUE-1504: `clippy`,
`linux-premerge`, `native-platforms`, `platform-corpus`, `affected-targets`,
`rue-program-digests`, `remote-execution`, `performance-staleness`, `release`,
and the sanitizer `valgrind` job.

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
  It runs `scripts/check-reproducible-compiler.sh` (RUE-617), which moves a
  wrapper-created `.buckconfig.local` link aside for the duration, refuses a
  hand-written one, and exports `RUE_NO_REMOTE_CACHE=1` so the wrapper does not
  relink: the reference and relocated candidate builds must be identically
  configured for the byte comparison to indict path/scheduling/
  environment leaks rather than configuration drift. Keeping that proof in an
  independent job lets the ordinary linux-x64 build and tests use the shared
  cache without changing the reproducibility contract.

`scripts/check-reproducible-build-metadata.py` is a separate, opt-in diagnostic
for investigating reproducibility below that final binary. It requires a clean
working tree, records `HEAD`, and archives that same tracked revision into two
differently named roots, giving each root an isolated Buck daemon/output tree,
and builds `//crates/rue:rue` with `--local-only` and
`--no-remote-cache`. Each archived root gets an ordinary empty
`.buckconfig.local`; every Buck wrapper invocation also points
`RUE_BUILDBUDDY_CONFIG` at a verified-nonexistent root-local path and removes
`BUILDBUDDY_API_KEY` from its environment. The sentinel is checked before and
after every build, query, ownership audit, and daemon shutdown. Thus the
wrapper finds no installed config to link, and would not replace the sentinel
anyway, so it cannot activate the central cache credential.
The resulting configured graph is rejected if it selects the remote-cache
platform. A configured `deps(//crates/rue:rue)` query scopes the
inventory: Rust library `.rlib`/`.rmeta` outputs, `rust_library` targets whose
`proc_macro` attribute is true, `rust_binary` targets whose crate is
`build_script_build`, the `OUT_DIR` and `rustc_flags` subtargets of reachable
Cargo build-script rules, and reachable `genrule` default outputs. The provider
set also describes optional products that the top-level build does not request,
so the inventory takes its intersection with outputs actually materialized by
this build. Each selected path must be bound by Buck's provider output to that
exact configured target, must live in its exact configured-target output
directory, must not pass through a `depslink` or `depsfull` input tree, and must
map back to the expected owner through `buck2 audit output`. The diagnostic
fails if any eligible graph contract or configured variant has no materialized,
owned output. Buck may execute Rust metadata actions without lazily
materializing their products, so the diagnostic separately builds the
`[check]` subtarget of every non-proc-macro Rust library already present in the
configured dependency graph with `--materializations all`, and fails if the
resulting inventory contains no `.rmeta`. It does not sweep `buck-out`, and it
excludes Buck command scaffolding and source trees.

The report directory contains both normalized manifests, raw observation files,
both queried graphs, and `comparison.json`. Normalized manifests omit raw
relocation-sensitive hashes, sizes, and filesystem mtimes; the observation
files retain those digests and numeric observations so the classification is
auditable, but archive names and payload observations never serialize literal
relocated roots. Archive members are
compared in order, including raw name encodings, header fields, alignment
padding, trailing bytes, and payloads; a whole-archive digest is the fail-closed
fallback. Filesystem mtimes are informational, while filesystem modes are
blocking metadata. Archive metadata, embedded relocated source/build paths,
path-only archive names or payloads, archive-format bytes, and other payload
changes are reported separately. Source-file mtimes and scheduling differ, but
both builds use the same `SOURCE_DATE_EPOCH`; paths are canonicalized on macOS
and both lexical and canonical spellings are normalized. Run it
directly when investigating build metadata:

```bash
scripts/check-reproducible-build-metadata.py
```

This diagnostic is intentionally not part of required CI and does not replace
`scripts/check-reproducible-compiler.sh`, which remains the final-artifact gate.

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

The same workflow carries `rue-program-warm`, ADR-0070's positive warm-cache
control (RUE-1405, extended by RUE-1406;
`scripts/check-rue-program-warm-cache.sh`). It cold-builds the two canary
`rue_program` targets and the nine staged CLI programs under the same nonce
mechanism, then from a *relocated* checkout root runs the canaries' four
consuming scenarios and rebuilds `//examples:cli-staged-programs`, failing unless every
scan/derive/compile action was a cache hit. Cross-root service is the property
the derive step's manifest re-anchoring exists to guarantee, and it is what
lets a `pull_request` run's compile be consumed by the `merge_group` run. Both
consumer shapes are covered deliberately: `rue_program_test` scenarios, and the
CLI corpus actions that declare the staged directory as an input.

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

Since RUE-1163 every heavy corpus is converted, including the two shell-script
harnesses (`//:reproducible-programs`, `//:oracle-diff-generated-smoke`) that
were held back until they read their repository paths through declared env
inputs. `scripts/ci-corpus-inventory` reports the current set from the graph:

```bash
scripts/ci-corpus-inventory            # every cached_corpus_suite, one per line
```

Exclusions are exact by construction — `--exclude //:cli-tests` names one
target, `--exclude-label rue_cli_shard` resolves through the graph. There is
deliberately no pattern form: exclusions are applied after the completeness
cross-check below, so a wildcard is the one way a corpus could leave the sweep
with every check still reporting green.

`scripts/ci-heavy-suite` also names the actions a lane really executed, from the
invocation's own event log, so the count above has an identity next to it:

```text
corpus lane: executed action root//:spec-tests-action (cfg#...) (rue_corpus spec-tests)
corpus lane: every build action was served from cache
```

### The undeclared-input safeguard (RUE-1222)

Consequence 1 above is the one with no in-band detector. A cache-served corpus
asserts a stamp; if the harness reads a path the action does not declare, that
stamp answers for a *different tree* and the suite passes without running. There
is no signal in the lane: the counters say "cached", the test says `Pass`, and
the wall time says the cache is working — which is exactly what a correct run
looks like. The only thing that distinguishes them is a run that actually
executes the harness against the current tree.

The weekly `correctness-repetitions.yml` workflow is that run:

- `repeat-cli-shards` runs each CLI shard five independent times (RUE-1159);
- `execute-every-corpus` runs every *other* converted corpus once, one runner
  each, taking its inventory from `scripts/ci-corpus-inventory` rather than from
  a list in the workflow. A corpus converted later is swept without an edit; an
  empty or unreadable inventory fails the job instead of passing vacuously; and
  the inventory is cross-checked against the `rue_heavy_suite` label set, so a
  heavy suite that is not a converted corpus fails rather than silently falling
  out of the sweep.

`//:cli-tests` is deliberately not swept. The four shards declare a strict
superset of its inputs — same harness, args, and `absolutize`, plus the shard
index and the weights file — and their union is the same case inventory, so the
repeated shards already expose anything it could fail to declare.

**What makes these runs execute is the runner, not a flag.** Each job starts with
an empty `buck-out` and no BuildBuddy secret, so there is nothing to serve it
from. `ci-heavy-suite` also passes `--no-remote-cache`, but that disables only
the *remote* cache: buck2's local DICE state and materialized outputs are
untouched, which is why repeating a target inside one workspace needs the
separate mechanism below. Do not read the flag as the guarantee.

What the sweep checks is the input *declaration*, which is a property of the rule
rather than of a configuration. It executes each corpus under
`prelude//platforms:default`, while `ci.yml` runs `//:release-smoke` under
`//platforms:release`; an input the harness reads but the action does not name
is missing in both, so either configuration exposes it. It does not exercise the
release-configured actions' own cache entries.

This bounds the exposure to one week; it does not remove it. The contract is
still that every path a harness reads at runtime reaches it through `env`, and
hence through `$(location ...)`. Nothing on the merge queue's critical path can
check that for you.

The nightly `release.yml` sweep is not a substitute either. It runs `//...` with
the cache provisioned, so any corpus in it may legitimately be served rather than
executed.

Two cautions from RUE-1222, both of which cost this lane real coverage:

- **Nothing turns red when a scheduled workflow fails.** Between RUE-1118 and
  RUE-1222 every scheduled run died in 18 seconds at argument parsing
  (`buck2 test --env` is rejected unless it follows `--`) and no one was told.
  After changing anything here, read the run list.
- **A repetition only counts if its action digest differs.** See below.

### Keeping the shard weights fresh (RUE-1222)

`shard-weights.json` (RUE-1158) balances the CLI shards from measured per-case
cost, and consequence 4 above keeps those measurements alive across a cache hit
by making them a declared output. What a hit replays, though, is the run that
*wrote* the entry: on a well-cached tree the balance is computed from numbers
nobody has re-measured.

That staleness degrades two things, not one. The obvious one is how evenly four
shards finish. The other is a gate: `scripts/cli-timeout-policy.py` derives each
shard's correctness deadline from the same weights, and
`//:cli-timeout-policy-validation` fails the build when a suite's
`timeout_seconds` in `BUCK` no longer covers the derived deadline. Weights are an
input to a required check, so "they only affect scheduling" is wrong.

The repetitions are where the corpus measures itself again, and each repetition
really executes because its index makes the action digest differ (below). Each
writes its own `case-timings-N.jsonl` into the
`correctness-repetitions-linux-x64-shard-N` artifact.

To refresh the weights, collect the files from **all four shard artifacts** and
pass every one:

```bash
scripts/generate-cli-shard-weights.py \
  --timings linux-x64=shard-0/case-timings-1.jsonl \
  --timings linux-x64=shard-1/case-timings-1.jsonl   # ... every file, every shard
```

The tool takes the median per case across all inputs, so several independent
repetitions are better evidence than one. It **replaces** `platforms.linux-x64`
with the union of what it is given rather than merging into what is there, and
each artifact holds only its own shard's cases — so one shard's files alone
would silently drop the other three shards' weights to the `common` fallback.

### Repeating a corpus so it actually runs (RUE-1222)

RUE-1159's shard repetitions are only evidence if each repetition executes. Two
mechanisms that look sufficient are not:

- `--no-remote-cache` disables the remote cache, not buck2's local DICE state.
  Inside one workspace, repetitions 2..N are served from repetition 1's result.
- An executor `--env` never reaches the harness: since RUE-1118 the harness runs
  inside a build action, and the test executor's environment is the stamp
  check's. Worse, `buck2 test --env` is rejected at argument parsing unless it
  follows `--`, which is what killed every scheduled run for weeks.

The index therefore travels as `-c rue.corpus_repetition=N`, which
`cached_corpus_suite` folds into the corpus action's environment. That makes each
repetition a distinct action digest buck2 must execute, and delivers the value to
the harness as well. It is injected only when non-empty, so an ordinary build's
digest is unchanged and no cache entry is invalidated. `ci-heavy-suite` passes it
to **both** its Buck invocations: the timings fetch must name the same
repetition's action, or it returns another repetition's measurements — or runs
the whole corpus again to produce them.

### The linux-x64 `local: 1` (RUE-1222, open)

On byte-identical trees the linux-x64 corpus lanes have reported exactly one
locally executed build action where the arm64 and macOS lanes report none. The
wall-time cost is negligible, but a standing platform-specific miss nobody can
name is the kind of thing that stops being one action.

It is **not identified**. The `Commands:` line counts the miss without naming
it, and the record that would name it is the BuildBuddy invocation view for
those runs. Three things are worth knowing before anyone repeats the search:

- **The cross-platform comparison is not like-for-like.** Every entry in
  `ci.yml`'s `platform-corpus` matrix is `ubuntu-latest`, and has been since
  before the conversion. arm64 and macOS reach the corpora through the
  `native-platforms` job instead, which runs
  `scripts/run-native-platform-corpus.sh` — `buck2 run` of the spec and CLI
  harnesses under `RUE_PLATFORM_CASE_SELECTION=native`. No `cached_corpus_suite`
  target is built there at all, so "zero local actions on arm64/macOS" is a
  different action graph reporting zero, not the same graph succeeding where
  linux-x64 fails.
- **The corpus action itself is not the candidate.** Its `local_only = True`
  governs where a *miss* executes, not whether the cache is consulted, and the
  RUE-1118 measurements show the corpora being served.
- **The Linux-only branches in the graph today are too new and too broad.**
  mimalloc's `zig_c_static_archive` and the `_COMPILER_ALLOCATOR_DEPS` link edge
  both postdate the observation, and both key on `prelude//os:linux` rather than
  on x86-64, so they would appear on the arm64 lane too.

Rather than guess further, `ci-heavy-suite` now prints the identity of every
locally executed action after each corpus run (see above). The next merge_group
run of an already-built tree answers this from the job log alone: read the
`corpus lane: executed action ...` lines in a linux-x64 corpus job, and the
target, configuration, and action category of the miss are all there. Until
that log exists, treat the count as unexplained rather than benign.

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
