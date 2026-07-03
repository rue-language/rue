# Remote build cache + execution (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). Every CI run
and every isolated worktree rebuilds unchanged crates. We use **BuildBuddy** (free
tier) for a shared remote action cache — and full remote **execution** was proven
to work locally: with the config below, the entire compiler built on BuildBuddy's
workers (171/175 actions remote, 0 local, ~103s cold).

> **Status: working (RUE-316 + RUE-320 landed).** The remote platform, the
> `$ORIGIN` toolchain-tree fix, and the linker are all in place. Full RE is
> functional: `buck2 build //crates/rue:rue --prefer-remote` runs the whole build
> on BuildBuddy's workers. The linker piece (RUE-320) is solved *globally* — the
> linux toolchains link via `cc -fuse-ld=lld` (`_LINUX_LINKER_FLAGS` in
> `toolchains/rust/BUCK` + `linker = "cc"` in `toolchains/BUCK`), so `cc` uses the
> rust-bundled lld and never falls back to `collect2 -> ld`. That keeps native CI
> green (no system-`ld` dependency) **and** makes RE work (`cc` is in the
> container, `clang++` was not) — no platform-scoping needed.

- **Remote action cache**: actions execute locally; the cache is shared across
  machines + daemon restarts. Can only affect *speed*, never correctness.
- **`--prefer-remote`**: full remote **execution** — compiles + links on
  BuildBuddy's container (~80 free-tier cores).

Tracking: RUE-316 (groundwork) + RUE-320 (linker), both landed.

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
5. **Linker** (RUE-320, landed): the remote worker needs the linker driver to be
   **`cc`** instead of `clang++` (`clang++` isn't in the container; `cc` is
   everywhere). The trap is that `cc` alone falls back to `collect2 -> ld`, which
   the hermetic CI sandbox lacks. The fix: `linker = "cc"` in `toolchains/BUCK`
   **plus** `_LINUX_LINKER_FLAGS = ["-Clink-arg=-fuse-ld=lld"]` on the linux rust
   toolchains (`toolchains/rust/BUCK`), so `cc` uses the rust-bundled lld and never
   touches system `ld`. Global and CI-safe — verified: native `./test.sh` Pass 24
   with the wrapper emitting `cc -fuse-ld=lld`, and a remote `--prefer-remote` link
   succeeds (`cc` in the container). No platform-scoping needed.

Changes 3 (`$ORIGIN` toolchain-tree) and 5 (linker) are global and CI-safe
(verified: native build + suite green). 1, 2, 4 live in the opt-in
`.buckconfig.local` / the `remote_cache` platform.

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
