# Remote build cache + execution (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). Every CI run
and every isolated worktree rebuilds unchanged crates. We use **BuildBuddy** (free
tier) for a shared remote action cache — and full remote **execution** was proven
to work locally: with the config below, the entire compiler built on BuildBuddy's
workers (171/175 actions remote, 0 local, ~103s cold).

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

- **Remote action cache**: actions execute locally; the cache is shared across
  machines + daemon restarts. Can only affect *speed*, never correctness.
- **`--prefer-remote`**: full remote **execution** — compiles + links on
  BuildBuddy's container (~80 free-tier cores). Blocked on RUE-320.

Tracking: RUE-316 (groundwork), RUE-320 (platform-scoped linker to finish RE).

## Local dev

```bash
cp .buckconfig.local.example .buckconfig.local
# paste your key (https://app.buildbuddy.io -> Settings -> API keys) into the
# x-buildbuddy-api-key line
```

`.buckconfig.local` is gitignored (holds the key) and buck2 layers it over
`.buckconfig` automatically. Without the file, nothing changes — the shared config
stays cache-agnostic.

## What it took (the non-obvious bits)

Getting from "no cache" to "full RE" hit several gaps; all are now handled, but
they're recorded here so a future config change doesn't silently regress:

1. **Connection knobs** (`.buckconfig.local`): the addresses need a **`grpc://`**
   scheme (buck2 rejects `grpcs://`; `tls = true` upgrades the transport), and
   **`[buck2] digest_algorithms = SHA256`** (BuildBuddy's CAS digest). Without
   these the RE client silently never connects (`remote: 0`, no error).
2. **`remote_enabled = True`** in `platforms/remote_cache.bzl`: OSS buck2 only
   opens the RE connection when remote is enabled — *even for cache-only use*. A
   pure `remote_enabled = False` cache config connects to nothing. So the platform
   enables remote but leans local (limited hybrid + fallback) for cache-mostly use.
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
The `cache-probe` workflow (`.github/workflows/cache-probe.yml`) writes a transient
`.buckconfig.local` from the secret, does a cold build then a clean-and-rebuild, and
reports buck2's `Commands: (cached / remote / local)` line. Run it with
`gh workflow run cache-probe.yml`. Once proven green with real hit-rates, the cache
config can be promoted into the main build and test jobs.

## Toward a fully hermetic linker

`linker = "cc"` still depends on the container having *a* C driver. The fully
hermetic version — mirroring the rust toolchain — would ship clang/lld as its own
toolchain so RE has zero container dependency. Not needed for correctness (lld
already does the linking); a future hardening if we make RE the default.
