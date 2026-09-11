# Explicit module manifest measurements

Measured on 2026-09-11 with the release compiler on macOS-26.6.2-arm64-arm-64bit-Mach-O.
Compiler SHA-256: `5de8974876c04a4778b968df06197e94e7ae120e27c61b8695902e49f563650f`.

The fixtures isolate source acquisition and parsing: the root returns 42, while
the imported helper bodies are not semantic roots. Each sample starts a fresh
compiler process and session; operating-system file caches are warm. Five
filesystem and manifest samples are interleaved per shape, size, and worker
count. Generation is measured separately with one worker. The compiler uses
`release_thin_lto`, the native AArch64 macOS target, internal linking, and Rue
`-O0`. This is an external prototype measurement, not an extension of the
established benchmark JSON provenance contract.

All 180 paired-mode compiler invocations and 18 manifest-order permutations
produced matching executable bytes and native exit status 42. The 45 generation
samples produced deterministic manifests. Retained-session reuse, body edits,
stale-input recovery, relocation, and cancellation have separate host tests;
these fresh-process timings do not measure retained-session latency.

All timings below are medians in milliseconds. Source loading includes session
construction and parsing. Inclusive timing rows may overlap and are not summed.

| Shape | Modules | Workers | Filesystem total | Manifest total | Filesystem source | Manifest source | Generation |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep_forward | 33 | 1 | 17.84 | 16.51 | 2.8 | 2.2 | 7.87 |
| deep_forward | 33 | 4 | 16.64 | 15.81 | 2.8 | 2.1 | 7.87 |
| deep_forward | 129 | 1 | 28.66 | 26.72 | 8.8 | 7.1 | 16.75 |
| deep_forward | 129 | 4 | 30.29 | 28.03 | 9.2 | 6.9 | 16.75 |
| deep_forward | 513 | 1 | 86.70 | 82.44 | 38.3 | 34.3 | 61.34 |
| deep_forward | 513 | 4 | 91.40 | 85.71 | 38.7 | 33.3 | 61.34 |
| deep_reverse | 33 | 1 | 15.82 | 15.36 | 2.5 | 2.0 | 7.06 |
| deep_reverse | 33 | 4 | 16.46 | 15.84 | 2.7 | 2.1 | 7.06 |
| deep_reverse | 129 | 1 | 29.21 | 27.26 | 8.9 | 7.2 | 16.76 |
| deep_reverse | 129 | 4 | 30.23 | 27.86 | 9.1 | 6.9 | 16.76 |
| deep_reverse | 513 | 1 | 85.44 | 82.70 | 36.9 | 34.1 | 60.40 |
| deep_reverse | 513 | 4 | 89.96 | 86.11 | 37.1 | 33.3 | 60.40 |
| wide | 33 | 1 | 16.10 | 15.68 | 2.6 | 2.0 | 7.21 |
| wide | 33 | 4 | 17.09 | 16.46 | 2.7 | 2.1 | 7.21 |
| wide | 129 | 1 | 28.80 | 26.88 | 9.0 | 7.1 | 17.26 |
| wide | 129 | 4 | 30.46 | 27.96 | 9.2 | 6.8 | 17.26 |
| wide | 513 | 1 | 86.91 | 82.95 | 38.1 | 34.5 | 61.17 |
| wide | 513 | 4 | 91.85 | 89.17 | 38.9 | 34.2 | 61.17 |

Generation is a migration/update cost, not part of the manifest-load column.
Small-case timings include process startup and output publication and are
sensitive to host scheduling. These results establish output equivalence for
the measured cases and characterize this host; they are not a cross-platform
performance guarantee or a threshold for changing default import behavior.

Reproduce with the commands in [the manifest guide](../process/module-manifest.md).
The harness retains individual samples, input hashes, and raw stdout/stderr.
The [summary data](module-manifest-measurements.json) records the compiler
configuration and medians used here.
