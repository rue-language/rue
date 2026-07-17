# RUE-790 borrowing RIR payload performance record

Measured 2026-07-16 for the RUE-790 pre-schema migration required by
[ADR-0056](../designs/0056-typed-ir-payload-schemas.md). This is a paired
baseline/candidate record; it does not update the historical performance
dashboard.

## Exact revisions and environment

- Baseline source revision: `50deb00da2e3aa65ae54d037619451c4882d1864`.
- Candidate source: that revision plus the frozen RUE-790 compiler patch whose
  SHA-256 is
  `0febfd0bb50a981949243ed4b7973f7663a6de2e6cecb5a6657136cef361790f`.
  The fingerprint is over the binary `git diff` for `crates/rue-air`,
  `crates/rue-rir`, `crates/rue/BUCK`, `crates/rue/src/main.rs`, and the new
  `crates/rue/src/allocation.rs`; the evidence document is deliberately not
  part of the compiler-source fingerprint.
- Uninstrumented production candidate binary SHA-256:
  `cac7d769fbb8c6aec8276dc3b7aed8a2f21f85451deaf82a157498dd858f9262`.
- Pristine upstream baseline production binary SHA-256:
  `4b84283fe49feeb012414318f374b029c2ed98c2faf9cd2476e1cdd2ceff1bb1`.
- Dedicated candidate `rue-benchmark` binary SHA-256:
  `997702303bb5e89c506bcee504e53489c32f9c0474d9ff91a403179390bfd055`.
- Dedicated baseline `rue-benchmark` binary SHA-256:
  `b29ba8979983adcf3f8ae46472b1bfac98c1de249802a5e63c105b243b272f85`.
- The baseline dedicated target received only the same benchmark-accounting
  patch used by the candidate. Its binary patch SHA-256 is
  `1a913e214e7b39a772fe24a58b81c80cd21d45649ead7e6bf7cabc7d35f86e4d`.
- Host: Apple arm64, macOS 26.5.2 (build 25F84), otherwise idle.
- Target/profile/linker: `aarch64-macos`, Buck2 default profile, internal
  linker.
- Toolchain: Rust 1.92.0 (`ded5c06cf`, 2025-12-08); Buck2
  `2026-06-30-c88d791e34884e58617b92d5b98c7f71faee823c`.

The compiler patch and all measured binaries were frozen before the series
below. No compiler source changed between fingerprint/build and measurement.

For publication, the reviewed commit was rebased onto upstream `17f09b56`.
The RUE-790 patch against that parent has SHA-256
`4d227466a20718a5cfe60e3182f0d61c6e0b745331f4fdb6f6e31e544828df8c` using
the same `git diff --binary` path set above. The only conflict was in
`crates/rue/src/main.rs`: upstream RUE-767 had deleted the legacy positional
input check adjacent to the benchmark allocation boundary. Resolution kept
upstream's deletion and the measured RUE-790 `allocation::pause()` boundary;
no measured RIR, AIR, allocation-accounting, or benchmark-target logic changed.
The publication patch also elides one iterator-implementation lifetime and
removes six needless borrows in tests to satisfy the Linux clippy gate; neither
change affects generated production code.
The paired `50deb00d` series remains the isolated before/after evidence for
RUE-790, while post-rebase correctness is covered by the publication checks.

## Workloads and protocol

| Workload | Source or generator | SHA-256 |
| --- | --- | --- |
| Many small functions/calls | `benchmarks/stress/many_functions.rue` | `6de992cedb83f6c5f73788574a994d4f74a8a1fa8c45697afffa325f2876e38f` |
| Match-heavy path bindings | generated `/tmp/rue790-match-path-bindings.rue` | `6f3d14e2af1740d240c863032262a60ad554a9396ebef910114e5da1498aa785` |
| Generic/comptime | `examples/generics.rue` | `6962376ddebf6f280ece700601549978203817f2160355d5b7fd66f511bb5225` |

The match workload has 400 functions. Every function matches the qualified
paths `Payload.Empty`, `Payload.One`, `Payload.Pair`, and `Payload.Quad` with
zero, one, two, and four bindings respectively. Its generator is:

```python
print("enum Payload { Empty, One(i32), Pair(i32, i32), Quad(i32, i32, i32, i32) }")
for index in range(400):
    print(f"fn _match_bind_{index:03}(value: Payload) -> i32 {{")
    print("    match value {")
    print("        Payload.Empty => 0,")
    print("        Payload.One(a) => a,")
    print("        Payload.Pair(a, b) => a + b,")
    print("        Payload.Quad(a, b, c, d) => a + b + c + d,")
    print("    }")
    print("}")
print("fn main() -> i32 { _match_bind_399(Payload.Quad(1, 2, 3, 4)) - 10 }")
```

Each revision received one discarded warmup for each workload. Seven measured
pairs followed. The chronological order alternated:

| Pair | Order |
| ---: | --- |
| 1 | baseline, candidate |
| 2 | candidate, baseline |
| 3 | baseline, candidate |
| 4 | candidate, baseline |
| 5 | baseline, candidate |
| 6 | candidate, baseline |
| 7 | baseline, candidate |

Values in every raw row below are pair 1 through pair 7; the table above gives
their chronological execution order. The command shape was:

```text
/usr/bin/time -l -o <accounting> <exact-rue-binary> \
  --benchmark-json <source> -o <unique-output> > <sample-json>
```

Wall time and peak RSS come from `/usr/bin/time -l`. Compiler and phase times
come from `--benchmark-json`. Medians and median absolute deviations (MAD) use
the seven raw values.

### Allocation boundary

Only `//crates/rue:rue-benchmark` installs the counting allocator. The ordinary
`//crates/rue:rue` target does not compile the allocation module, does not set a
global allocator, and contains no allocation-counter atomic load. For
`--benchmark-json`, the dedicated target resets immediately before the
canonical `compile` root begins and enables atomic counters only
while one of that root's intervals is active. The boundary includes import
discovery, parsing, RIR, semantic analysis, CFG construction, code generation,
and linking; CLI setup, output-file writing, diagnostics printing, and JSON
formatting are outside it. Successful `alloc`, `alloc_zeroed`, and `realloc`
calls count as allocation calls, and requested bytes include each successful
allocation size or reallocation's new size. Counting is disabled for ordinary
compiler invocations. Identical instrumentation is compiled into the two
dedicated benchmark binaries.

## Dedicated benchmark-target results

| Workload | Revision | wall s, median ± MAD | peak RSS bytes, median ± MAD | compiler ms, median ± MAD | allocations, median ± MAD | requested bytes, median ± MAD |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Many functions | baseline | 0.33 ± 0.00 | 50,511,872 ± 163,840 | 315.606 ± 0.796 | 725,309 ± 3 | 108,132,487 ± 2,008 |
| Many functions | candidate | 0.33 ± 0.00 | 50,200,576 ± 212,992 | 318.689 ± 4.329 | 714,304 ± 4 | 107,921,647 ± 3,688 |
| Match/path bindings | baseline | 0.09 ± 0.00 | 25,149,440 ± 16,384 | 79.780 ± 0.153 | 101,860 ± 2 | 18,781,606 ± 376 |
| Match/path bindings | candidate | 0.09 ± 0.00 | 25,247,744 ± 49,152 | 79.861 ± 0.093 | 99,034 ± 4 | 18,749,565 ± 1,232 |
| Generic/comptime | baseline | 0.02 ± 0.00 | 21,561,344 ± 32,768 | 10.485 ± 0.146 | 46,877 ± 1 | 6,734,894 ± 208 |
| Generic/comptime | candidate | 0.02 ± 0.00 | 21,594,112 ± 49,152 | 10.396 ± 0.121 | 46,749 ± 2 | 6,727,810 ± 416 |

Allocation non-increase is stronger than a median-only result: the maximum
candidate allocation count is below the minimum baseline count in every
workload.

| Workload | baseline minimum | candidate maximum | proof |
| --- | ---: | ---: | --- |
| Many functions | 725,303 | 714,311 | candidate maximum is 10,992 lower |
| Match/path bindings | 101,857 | 99,044 | candidate maximum is 2,813 lower |
| Generic/comptime | 46,876 | 46,753 | candidate maximum is 123 lower |

## Uninstrumented production wall time and RSS

The production comparison used the pristine upstream `50deb00d` `rue` binary
and the final candidate's ordinary `//crates/rue:rue` binary. Neither binary
contains the counting allocator. It used the same warmup, seven alternating
pairs, workloads, command shape, and pair order documented above.

| Workload | Revision | wall s, median ± MAD | peak RSS bytes, median ± MAD |
| --- | --- | ---: | ---: |
| Many functions | baseline | 0.31 ± 0.00 | 52,723,712 ± 245,760 |
| Many functions | candidate | 0.31 ± 0.00 | 53,182,464 ± 294,912 |
| Match/path bindings | baseline | 0.09 ± 0.00 | 26,263,552 ± 131,072 |
| Match/path bindings | candidate | 0.09 ± 0.00 | 26,476,544 ± 81,920 |
| Generic/comptime | baseline | 0.02 ± 0.00 | 21,954,560 ± 32,768 |
| Generic/comptime | candidate | 0.02 ± 0.00 | 21,938,176 ± 32,768 |

Raw samples, pair 1 through pair 7:

| Workload/revision | wall seconds | peak RSS bytes |
| --- | --- | --- |
| many/baseline | 0.31, 0.31, 0.30, 0.31, 0.31, 0.32, 0.31 | 52723712, 52330496, 52740096, 52969472, 52576256, 53067776, 52445184 |
| many/candidate | 0.31, 0.31, 0.31, 0.30, 0.32, 0.31, 0.31 | 52150272, 53198848, 51806208, 53477376, 51855360, 53215232, 53182464 |
| match/baseline | 0.09, 0.09, 0.09, 0.09, 0.09, 0.09, 0.09 | 26132480, 26099712, 26263552, 26378240, 26394624, 25804800, 26296320 |
| match/candidate | 0.09, 0.09, 0.09, 0.09, 0.09, 0.09, 0.09 | 26476544, 26509312, 26312704, 26558464, 26361856, 26542080, 26312704 |
| generic/baseline | 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02 | 21938176, 21938176, 22036480, 21889024, 21954560, 22003712, 21987328 |
| generic/candidate | 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02 | 21790720, 21954560, 21872640, 21889024, 21938176, 21954560, 21970944 |

Wall medians are unchanged. Many-functions RSS increases by 0.87%, and
match/path-binding RSS increases by 0.81%; both are below their 2% gates.
Generic/comptime RSS decreases by 0.07%. The uninstrumented production series
passes the ADR-0056 wall/RSS gate without relying on symmetric allocator
overhead.

The target separation was also checked directly:

```text
normal rue --benchmark-json: compiler_allocations key absent
nm <normal-rue> | rg CountingAllocator: no matches
dedicated rue-benchmark --benchmark-json: compiler_allocations key present
nm <rue-benchmark> | rg CountingAllocator: allocator symbols present
```

Thus an ordinary compiler invocation uses Rust's unwrapped platform allocator
and incurs no allocation-counter load or instrumentation branch.

## Raw compiler samples

### Many functions

| Revision | wall s | peak RSS bytes | compiler ms | allocations | requested bytes |
| --- | --- | --- | --- | --- | --- |
| baseline | 0.33, 0.33, 0.33, 0.33, 0.33, 0.33, 0.33 | 50511872, 49889280, 50659328, 50675712, 49790976, 50495488, 50790400 | 315.385, 314.810, 315.601, 316.548, 315.606, 318.355, 317.980 | 725312, 725312, 725321, 725308, 725303, 725309, 725308 | 108132911, 108134511, 108140983, 108131279, 108125639, 108132487, 108130479 |
| candidate | 0.33, 0.34, 0.33, 0.33, 0.33, 0.36, 0.33 | 50544640, 50200576, 50626560, 50905088, 49987584, 50036736, 50036736 | 314.994, 323.887, 318.689, 318.876, 314.360, 346.584, 314.219 | 714303, 714308, 714297, 714304, 714302, 714309, 714311 | 107920839, 107925335, 107916391, 107921647, 107920431, 107927287, 107928103 |

### Match/path bindings

| Revision | wall s | peak RSS bytes | compiler ms | allocations | requested bytes |
| --- | --- | --- | --- | --- | --- |
| baseline | 0.09, 0.09, 0.09, 0.09, 0.09, 0.09, 0.09 | 25165824, 25100288, 25116672, 25149440, 25149440, 25116672, 25165824 | 79.627, 80.042, 79.925, 80.262, 79.780, 79.538, 79.666 | 101865, 101860, 101864, 101858, 101860, 101861, 101857 | 18784062, 18782422, 18781654, 18781606, 18781022, 18781230, 18781398 |
| candidate | 0.09, 0.09, 0.09, 0.09, 0.09, 0.09, 0.09 | 25247744, 24772608, 25296896, 25296896, 25247744, 25280512, 24805376 | 79.941, 79.861, 80.029, 80.007, 79.591, 79.847, 79.768 | 99028, 99040, 99044, 99034, 99030, 99034, 99038 | 18745917, 18749613, 18752045, 18747365, 18748333, 18749565, 18750197 |

### Generic/comptime

| Revision | wall s | peak RSS bytes | compiler ms | allocations | requested bytes |
| --- | --- | --- | --- | --- | --- |
| baseline | 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02 | 21512192, 21594112, 21561344, 21528576, 21643264, 21561344, 21495808 | 10.631, 10.305, 10.485, 10.370, 10.668, 11.037, 10.415 | 46878, 46878, 46879, 46877, 46877, 46876, 46877 | 6735102, 6735102, 6735310, 6734894, 6734894, 6734686, 6734894 |
| candidate | 0.02, 0.02, 0.02, 0.02, 0.06, 0.02, 0.02 | 21610496, 21168128, 21594112, 21544960, 21594112, 21184512, 21643264 | 10.390, 10.594, 10.396, 10.692, 10.359, 10.274, 10.914 | 46747, 46749, 46747, 46749, 46750, 46753, 46752 | 6727394, 6727810, 6727394, 6727810, 6728018, 6728642, 6728434 |

## Phase timings

Phase medians ± MAD, in milliseconds:

| workload/revision | semantic astgen | RIR declaration index | sema | CFG | codegen |
| --- | ---: | ---: | ---: | ---: | ---: |
| many/baseline | 7.715 ± 0.156 | 1.435 ± 0.016 | 93.163 ± 0.160 | 29.891 ± 0.278 | 69.947 ± 0.645 |
| many/candidate | 7.571 ± 0.063 | 1.444 ± 0.009 | 93.146 ± 0.863 | 29.764 ± 0.086 | 70.059 ± 0.548 |
| match/baseline | 6.752 ± 0.014 | 0.582 ± 0.005 | 13.969 ± 0.113 | 0.267 ± 0.016 | 0.453 ± 0.005 |
| match/candidate | 6.874 ± 0.042 | 0.606 ± 0.016 | 14.149 ± 0.056 | 0.273 ± 0.013 | 0.458 ± 0.005 |
| generic/baseline | 0.235 ± 0.004 | 0.085 ± 0.001 | 0.956 ± 0.008 | 0.317 ± 0.015 | 0.764 ± 0.030 |
| generic/candidate | 0.234 ± 0.003 | 0.086 ± 0.002 | 0.976 ± 0.015 | 0.324 ± 0.019 | 0.754 ± 0.005 |

Raw phase samples follow; each row is pair 1 through pair 7.

### Many-functions phases

| Revision/phase | raw ms |
| --- | --- |
| baseline/semantic astgen | 7.720, 7.715, 7.671, 7.870, 8.221, 7.533, 7.434 |
| baseline/RIR declaration index | 1.413, 1.427, 1.416, 1.460, 1.450, 1.440, 1.435 |
| baseline/sema | 91.910, 92.392, 93.323, 93.229, 94.007, 93.014, 93.163 |
| baseline/CFG | 29.635, 30.377, 29.748, 28.341, 29.891, 30.168, 30.302 |
| baseline/codegen | 70.592, 69.947, 68.677, 70.326, 68.007, 69.911, 71.076 |
| candidate/semantic astgen | 7.785, 8.031, 7.468, 7.536, 7.571, 7.513, 7.634 |
| candidate/RIR declaration index | 1.444, 1.456, 1.453, 1.452, 1.443, 1.418, 1.427 |
| candidate/sema | 93.061, 94.232, 94.009, 95.043, 92.958, 93.146, 92.128 |
| candidate/CFG | 29.791, 29.622, 29.712, 29.883, 29.764, 29.850, 29.589 |
| candidate/codegen | 70.059, 70.296, 71.162, 69.512, 69.510, 99.262, 69.155 |

### Match/path-binding phases

| Revision/phase | raw ms |
| --- | --- |
| baseline/semantic astgen | 6.811, 6.758, 6.738, 6.752, 6.739, 6.685, 6.947 |
| baseline/RIR declaration index | 0.577, 0.612, 0.574, 0.584, 0.594, 0.580, 0.582 |
| baseline/sema | 14.083, 13.969, 13.962, 14.551, 13.856, 13.955, 14.090 |
| baseline/CFG | 0.280, 0.250, 0.341, 0.251, 0.336, 0.267, 0.251 |
| baseline/codegen | 0.470, 0.449, 0.448, 0.511, 0.453, 0.452, 0.476 |
| candidate/semantic astgen | 6.741, 6.885, 6.990, 6.878, 6.874, 6.786, 6.831 |
| candidate/RIR declaration index | 0.602, 0.606, 0.588, 0.600, 0.622, 0.633, 0.629 |
| candidate/sema | 13.930, 14.149, 14.001, 14.232, 14.171, 14.093, 14.197 |
| candidate/CFG | 0.356, 0.343, 0.256, 0.273, 0.269, 0.278, 0.260 |
| candidate/codegen | 0.470, 0.450, 0.465, 0.453, 0.458, 0.456, 0.458 |

### Generic/comptime phases

| Revision/phase | raw ms |
| --- | --- |
| baseline/semantic astgen | 0.231, 0.231, 0.208, 0.235, 0.236, 0.249, 0.237 |
| baseline/RIR declaration index | 0.085, 0.086, 0.077, 0.085, 0.083, 0.094, 0.084 |
| baseline/sema | 0.956, 0.948, 0.943, 0.961, 0.963, 1.054, 0.932 |
| baseline/CFG | 0.306, 0.293, 0.333, 0.317, 0.338, 0.326, 0.266 |
| baseline/codegen | 0.757, 0.712, 0.794, 0.764, 0.834, 0.796, 0.744 |
| candidate/semantic astgen | 0.244, 0.234, 0.232, 0.235, 0.232, 0.231, 0.243 |
| candidate/RIR declaration index | 0.080, 0.086, 0.084, 0.086, 0.088, 0.087, 0.090 |
| candidate/sema | 0.991, 0.949, 0.976, 0.978, 0.976, 0.950, 1.014 |
| candidate/CFG | 0.331, 0.311, 0.324, 0.355, 0.305, 0.297, 0.379 |
| candidate/codegen | 0.751, 0.749, 0.754, 0.804, 0.844, 0.753, 0.856 |

## Cold versus reused CompilerSession

The repository's `rue-compiler-session-bench` ran with 64 modules, one warmup,
and seven measured iterations for each revision:

```text
./buck2 run //crates/rue-compiler-session-bench:rue-compiler-session-bench \
  -- --modules 64 --warmup 1 --iterations 7
```

`cold` constructs and queries a new session. `exact_noop` repeats the same
snapshot in that session. The benchmark structurally asserts that the reused
case performs zero lexer/parser, RIR, and semantic executions and records RIR
and semantic reuse instead. Times are milliseconds.

| Revision/scenario | raw samples | median ± MAD |
| --- | --- | ---: |
| baseline/cold | 11.580, 11.541, 11.856, 11.479, 11.563, 11.544, 11.676 | 11.563 ± 0.022 |
| candidate/cold | 11.620, 11.487, 11.736, 11.266, 11.540, 11.266, 11.373 | 11.487 ± 0.133 |
| baseline/exact noop reuse | 1.575, 1.596, 1.626, 1.594, 1.580, 1.533, 1.560 | 1.580 ± 0.015 |
| candidate/exact noop reuse | 1.596, 1.566, 1.564, 1.521, 1.582, 1.550, 1.583 | 1.566 ± 0.016 |

Both cold and reused-session medians decrease. The reused path remains a true
query reuse rather than a hidden recomputation.

## Focused traversal allocation gate

The RIR unit test wraps construction and complete consumption of each of the
eight migrated payload getters with a thread-local counting allocator. Every
family reports exactly zero allocations. Construction is included; the test
does not stop after creating an iterator. This complements the whole-compiler
counts above.

## Verdict

ADR-0056's noisy-metric allowance is the larger of 2% of the baseline median
or three times the larger MAD. Dedicated-target wall medians are unchanged.
The largest dedicated-target RSS increase is match/path bindings: 98,304
bytes, or 0.39%, below the 2% gate of 502,989 bytes. The largest compiler-span
increase is many-functions at 3.082 ms (0.98%), below both its 2% gate and its
noise allowance; match/path bindings increases by 0.082 ms (0.10%), and
generic/comptime decreases. The separate uninstrumented production series has
unchanged wall medians; its RSS changes are +0.87%, +0.81%, and -0.07%, all
within the gate.

Whole-compiler allocation counts decrease for all workloads, with disjoint raw
baseline/candidate ranges. Focused payload traversals remain exactly zero
allocation. Cold and reused-session medians both decrease. The production
binary has no counting allocator or atomic allocation path. RUE-790 therefore
passes the requested wall-time, RSS, allocation, phase-timing, variable
path-binding, session-reuse, and zero-production-instrumentation gates.
