# BuildBuddy setup audit, 2026-09-20

The remote cache is serving the current build graph. The remaining avoidable
work found in this audit is duplicate Rust standard-library extraction
(RUE-2292). A separate wrapper defect makes the advertised cache-free control
ineffective in already-configured worktrees (RUE-2291).

Source baseline: `d20a9a5f3b137f0547b4e2ae7706f6394df27e33`.

## Current CI evidence

The five latest cache-probe runs, September 14, 7, 4 and August 31, 24, all
passed. The [September 14 probe](https://github.com/rue-language/rue/actions/runs/34832543873)
used a unique nonce and cleared local outputs between builds. Its uploaded
`cold.log` and `warm.log` report:

| Release graph | Wall time | Cached | Local | Downloaded |
| --- | ---: | ---: | ---: | ---: |
| Cold | 42m21.56s | 278 | 713 | 266 MiB |
| Warm | 39.94s | 950 | 41 | 519 MiB |

These are a cache-population control, not an ordinary PR latency benchmark:
the probe builds the full release graph, and its nonce deliberately invalidates
Rust actions. A higher warm download volume is compatible with much less work.

The [September 20 merge-group run](https://github.com/rue-language/rue/actions/runs/35528081649)
shows the ordinary cache path working as well:

| Step | Cached / commands | Local | Downloaded |
| --- | ---: | ---: | ---: |
| Premerge build | 965 / 1002 | 37 | 562 MiB |
| Release smoke | 458 / 465 | 7 | 237 MiB |
| Each CLI shard | 492 / 495 | 3 | 156 MiB |
| Each oracle-diff lane | 410 / 413 | 3 | 156 MiB |

The corpus lanes identify their three local actions: rustc, rust-std and Zig
distribution extraction. Keeping unpacked toolchain trees out of the CAS is
the intentional RUE-2003 reliability tradeoff, still present in
`toolchains/distribution.bzl`. Re-enabling their uploads would revive the
large multi-blob materialization path that failed repeatedly.

The premerge job took 5m39s, but its build step finished at 18:09:34 UTC;
premerge tests finished at 18:12:28 and cross-backend tests at 18:14:21.
This sample does not support attributing the remaining critical path to
BuildBuddy cache misses or changing the ADR-0069 remote-execution policy.

## Duplicate standard-library distributions

A fresh-output native compiler build on macOS ARM64 took 10.0 seconds with
392 cached and six local actions. Buck trace
`46a1fcb2-8f1e-40d6-8509-e71fb225a4fb` records 185 MiB downloaded: 168 MiB
over HTTP and 17 MiB from remote execution storage. This is one observation,
not a controlled wall-time comparison.

The graph contained the same macOS Rust standard library twice: once in the
compiler's execution configuration and once in the runtime's target
configuration. Both extracted trees were 134,065,935 bytes, with identical
output digests; each consumed about 1.1 seconds extracting and 0.5 seconds
hashing. Each also had its own 27,221,324-byte archive output.

`crates/rue-runtime/runtime.bzl` declared `target_std` as a target dependency,
although all six runtime declarations name an exact archive and target triple.
Execution-scoping this dependency shares extraction with the Rust toolchain
and between debug and release builds. The runtime itself remains
target-configured: hosted/freestanding selection, target triples and allocator
debug assertions retain their meaning.

The baseline debug and release compiler closures collectively contain seven
configured rust-std nodes. Sharing the distributions reduces this to three.
On the measured Mac, that removes one duplicate archive plus tree from a
fresh debug build (153.8 MiB of logical output), and four from a combined
debug/release build (676.5 MiB). These are eliminated output bytes, not measured
physical disk reclamation or a wall-time speedup. Existing outputs remain until
Buck's normal cleanup reclaims them. Changing sysroot input paths also causes
a one-time cache-key change for the consuming runtime actions.

Validation rebuilt all six hosted/freestanding runtime archives and compared
their SHA-256 hashes with the baseline: all six were byte-identical.
`//crates/rue-runtime:runtime-archives-test` passed its architecture, object
format, exports and imports checks. Configured graph queries confirmed the
same three distribution nodes in both modes, and action queries retained
`-Cdebug-assertions=yes` for debug allocators and `no` for release allocators.

The [PR's macOS CI run](https://github.com/rue-language/rue/actions/runs/35529704593)
also retained 392 cache hits while reducing local actions from six to five
and downloads from 171 MiB to 145 MiB compared with the September 20 baseline
above. This confirms one less distribution download and extraction in CI.

## Measurement and configuration controls

`RUE_NO_REMOTE_CACHE=1` previously skipped config provisioning but left an
existing `.buckconfig.local` active. It therefore could not establish a
cache-free comparison. The wrapper now enforces local execution and disables
remote cache reads and writes for execution commands, without moving user
configuration. Local incremental state still requires a separate fresh-output
control when measuring a genuinely cold build.

The audited developer's installed private config also predated RUE-2003's
1,000,000-byte batch limit and 16,000,000-byte decode ceiling. Those two settings
were updated while preserving the key and other settings. The running daemon
kept its old values until restarted; explicit, non-secret `audit config` queries
then verified the new values. Existing daemons in other worktrees were not
interrupted. This is a reliability correction, not a measured speedup.

Large-blob compression is already negotiated automatically by the
[pinned Buck client](https://github.com/facebook/buck2/blob/1560aca2002865cd73d7cafb22c705cfb640b2bc/remote_execution/oss/re_grpc/src/client.rs#L279).
Adding Bazel compression flags to Buck is not an applicable optimization.
BuildBuddy account quotas, billing and worker utilization were not inspected:
the browser required a new GitHub authorization. Client-side transfer and
action counts above must not be presented as account usage or remaining quota.
