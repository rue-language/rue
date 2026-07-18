# Remote build cache + execution (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). Every CI run
and every isolated worktree rebuilds unchanged crates. We use **BuildBuddy** (free
tier) for a shared remote action cache. Remote execution experiments established
the remaining toolchain requirements, but complete remote builds are not a
supported workflow while RUE-320 remains open.

> **Status (RUE-316 landed the groundwork; RUE-320 finishes it).** The remote
> platform + the `$ORIGIN` toolchain-tree fix are in place, but full RE is **not
> enabled by default and not yet landable as-is**. It needs the linker driver to
> be `cc` (so lld, not the absent `clang++`, links on the remote worker) — and
> that override can only be applied to the **remote platform**, not globally:
> making it the toolchain-wide default (`linker = "cc"` in `toolchains/BUCK`)
> breaks native CI, whose system builds rely on the default linker path
> (`collect2: cannot find 'ld'`). Platform-scoped linker selection is tracked as
> **RUE-320**. Until then the `remote_cache` platform is opt-in/experimental and
> its remote-execution actions require that linker override locally.

- **Remote action cache**: the repository `./buck2` wrapper supplies
  `--prefer-local`, so cache misses execute locally while hits are shared across
  machines + daemon restarts. The cache can only affect *speed*, never
  correctness.
- **`--prefer-remote`**: full remote **execution** — compiles + links on
  BuildBuddy's container (~80 free-tier cores). Blocked on RUE-320.

Tracking: RUE-316 (groundwork), RUE-320 (platform-scoped linker to finish RE).

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

The setup is for the shared **action cache**. Do not add `--prefer-remote` to
normal commands: full remote execution remains blocked on RUE-320. The checked-in
`.buckconfig.local.example` remains a reference for the generated configuration,
not the recommended per-worktree setup.

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
3. **rustc under RE** (`toolchains/rust/defs.bzl`): rustc finds `librustc_driver.so`
   via its native `$ORIGIN/../lib` RPATH, which only resolves if the whole toolchain
   tree is materialized on the remote worker. The `compiler`/`rustdoc` RunInfo carry
   the full distribution as a hidden input so RE uploads it co-located. This is the
   relocatable, canonical fix — *not* an absolute-path `LD_LIBRARY_PATH` hack.
4. **Container** (`platforms/remote_cache.bzl`): pinned to `rbe-ubuntu22-04`
   (Python 3.10 — the prelude's rustc wrapper needs ≥3.9; the default image ships
   3.6).
5. **Linker** (RUE-320, NOT landed): the remote worker needs the linker driver to
   be **`cc`** instead of `clang++` (lld — `-fuse-ld=lld`, shipped with rust — does
   the real linking; the driver just needs to exist, and `cc` is everywhere while
   `clang++` is not). This was proven with `linker = "cc"` in `toolchains/BUCK`, but
   that override is **global** and breaks native CI (`cc` → `collect2` → `cannot
   find 'ld'` when the hermetic lld flags aren't on the command). It has to be
   **scoped to the remote platform** instead — tracked as RUE-320.

Change 3 (`$ORIGIN` toolchain-tree) is global and local-safe (verified: local
build + suite green). 1, 2, 4 live in the opt-in `.buckconfig.local` / the
`remote_cache` platform. 5 is the remaining blocker for a default-on RE.

## CI

CI reads the key from the `BUILDBUDDY_API_KEY` repo secret (never from a file).
The cache is provisioned (RUE-1006) in the `CI` workflow's `clippy`, `release`,
and non-x64 `test` jobs, and in the sanitizer `valgrind` job, via
`scripts/provision-build-cache install && apply` gated on secret presence.

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
- **The linux-x64 `test` lane is intentionally cache-free.** It runs
  `scripts/check-reproducible-compiler.sh` (RUE-617), which hard-errors on a
  `.buckconfig.local`: the reference and relocated candidate builds must be
  identically configured for the byte comparison to indict path/scheduling/
  environment leaks rather than configuration drift. Warming that lane by
  removing the config before the repro step is a possible follow-up once the
  queue has demonstrated real hit-rates.

The `cache-probe` workflow (`.github/workflows/cache-probe.yml`) remains the
measurement tool: it writes a transient config from the secret, does a cold
build then a clean-and-rebuild, and reports buck2's `Commands: (cached /
remote / local)` line. Run it with `gh workflow run cache-probe.yml`.

## Toward a fully hermetic linker

`linker = "cc"` still depends on the container having *a* C driver. The fully
hermetic version — mirroring the rust toolchain — would ship clang/lld as its own
toolchain so RE has zero container dependency. Not needed for correctness (lld
already does the linking); a future hardening if we make RE the default.
