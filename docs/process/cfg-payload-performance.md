# CFG typed-payload performance record (RUE-840)

This is the phase-local paired record for the CFG migration in ADR-0056. The
cross-phase allocation, side-table-capacity, focused builder, and clean and
incremental build matrix remains the RUE-843 integration gate after the RIR,
AIR, and CFG migrations are all available. This record does not substitute for
that final matrix.

RUE-843 must measure and pass all of these still-mandatory integration gates:

- whole-workload allocation counts for every representative compiler workload;
- per-family logical bytes, allocated-capacity bytes, total side-table bytes,
  nonempty envelopes where applicable, and peak staging bytes;
- focused iterator and atomic-builder throughput plus allocations for every
  migrated RIR, AIR, and CFG family; and
- at least five alternating clean-build samples plus deterministic incremental
  RIR, AIR, and CFG rebuild samples with rebuilt action counts.

RUE-840 does not claim completion of that full matrix.

## Reproduction identity

- Baseline revision: `50deb00da2e3aa65ae54d037619451c4882d1864`
- Candidate: the same revision plus the RUE-840 code patch whose binary Git
  diff (including the new `payload.rs`) has SHA-256
  `4b561a1aab5d3334c6c7cf0627ee589d7c0665c2a06ad723c9661d3e9d783aab`.
  The benchmark record itself is excluded from that digest.
- Host: Apple ARM64 T8132; macOS 26.5.2 (25F84), Darwin 25.5.0.
- Toolchain: `rustc 1.92.0 (ded5c06cf 2025-12-08)`; Buck2
  `2026-06-30-c88d791e34884e58617b92d5b98c7f71faee823c`.
- Target/profile: native `aarch64-apple-darwin`, repository default compiler
  profile, Rue program optimization `-O0`.
- Protocol: separate baseline and candidate worktrees at the identity above;
  one warmup; seven fresh compiler processes in alternating baseline/candidate
  order; `/usr/bin/time -l` for wall time and peak RSS; compiler
  `--benchmark-json` for inclusive compile and `cfg_construction` timings.
  Medians and median absolute deviations (MAD) are reported below.

For publication, the reviewed commit was rebased without conflicts onto
upstream `2fee31bfded2b44d96526357a049a7bb98cb1ebb` after RUE-790 merged. The
complete RUE-840 binary diff against that parent, excluding this benchmark
record, has SHA-256
`bf24d521f1dd44f18b39e54f6c39f144291b42d29ab34871865aa42a8118c7ac`.
The paired `50deb00d` record remains the isolated CFG migration measurement;
the conflict-free rebase required the newly landed RUE-926 loop infrastructure
to be reconciled with RUE-840's typed owner APIs, and the post-rebase quick and
focused suites are the publication correctness evidence.

## Workloads and result

| Workload (SHA-256) | Metric | Baseline median ± MAD | Candidate median ± MAD | Delta | ADR-0056 gate |
|---|---:|---:|---:|---:|---:|
| `many_functions.rue` (`6de992ce…e38f`) | wall ms | 307.627 ± 1.221 | 305.051 ± 0.922 | -0.84% | pass |
| | peak RSS bytes | 52,887,552 ± 81,920 | 52,887,552 ± 163,840 | 0.00% | pass |
| | compile ms | 289.571 ± 1.448 | 287.094 ± 0.758 | -0.86% | pass |
| | CFG ms | 27.466 ± 0.231 | 27.470 ± 0.064 | +0.02% | pass |
| `control_flow.rue` (`7bf2abe8…f58`) | wall ms | 265.392 ± 4.091 | 268.689 ± 1.673 | +1.24% | pass |
| | peak RSS bytes | 60,915,712 ± 393,216 | 61,030,400 ± 229,376 | +0.19% | pass |
| | compile ms | 244.932 ± 4.538 | 248.385 ± 0.757 | +1.41% | pass |
| | CFG ms | 23.469 ± 2.383 | 25.134 ± 1.412 | +7.09% | pass (noise bound) |
| `representative/main.rue` (`c41fad9f…745`) | wall ms | 24.704 ± 0.508 | 24.915 ± 0.146 | +0.85% | pass |
| | peak RSS bytes | 22,396,928 ± 32,768 | 22,544,384 ± 49,152 | +0.66% | pass |
| | compile ms | 11.886 ± 0.270 | 11.780 ± 0.052 | -0.89% | pass |
| | CFG ms | 0.264 ± 0.035 | 0.267 ± 0.029 | +1.33% | pass |

The gate is `candidate - baseline <= max(2% of baseline, 3 * max(MAD))`.
Every measured metric passes. The apparently large percentage for the
control-flow CFG span is a 1.665 ms change against a permitted 7.150 ms noise
bound.

## Raw alternating samples

Each row lists baseline then candidate samples in execution order.

### Many small functions and calls

- wall ms: B `[306.406, 304.402, 303.966, 307.627, 308.231, 309.945, 307.808]`;
  C `[303.956, 303.460, 311.996, 305.487, 305.051, 305.710, 304.129]`
- peak RSS bytes: B `[52969472, 52609024, 53002240, 51937280, 52838400, 52887552, 52920320]`;
  C `[52822016, 53051392, 53313536, 52969472, 52232192, 52887552, 52461568]`
- compile ms: B `[288.123, 287.272, 286.341, 289.887, 289.571, 292.497, 289.862]`;
  C `[286.619, 285.709, 294.581, 287.852, 287.094, 288.134, 286.677]`
- CFG ms: B `[27.115, 27.626, 27.011, 27.722, 27.466, 27.622, 27.234]`;
  C `[27.330, 27.406, 27.530, 27.524, 27.658, 27.470, 27.123]`

### Match/control-flow heavy

- wall ms: B `[265.392, 263.405, 276.273, 276.245, 268.093, 261.302, 258.765]`;
  C `[268.229, 273.257, 268.689, 270.362, 273.984, 264.897, 268.370]`
- peak RSS bytes: B `[60080128, 60276736, 60915712, 60538880, 61161472, 61308928, 61505536]`;
  C `[61685760, 61259776, 61423616, 60456960, 60899328, 61030400, 61030400]`
- compile ms: B `[244.932, 243.090, 254.081, 254.928, 247.129, 240.394, 238.096]`;
  C `[247.628, 252.474, 248.385, 248.812, 253.273, 243.958, 247.710]`
- CFG ms: B `[25.853, 20.958, 24.787, 25.678, 20.655, 23.469, 20.356]`;
  C `[21.791, 21.353, 23.616, 26.341, 26.546, 25.247, 25.134]`

### Representative specialization

- wall ms: B `[24.196, 26.425, 25.222, 25.425, 24.395, 24.579, 24.704]`;
  C `[25.808, 25.061, 24.993, 24.350, 24.786, 24.454, 24.915]`
- peak RSS bytes: B `[22396928, 22249472, 22380544, 22429696, 22446080, 22429696, 22347776]`;
  C `[22528000, 22364160, 22626304, 22593536, 22544384, 22544384, 22495232]`
- compile ms: B `[11.915, 12.364, 11.886, 11.944, 11.370, 11.615, 11.557]`;
  C `[11.884, 11.780, 11.828, 11.557, 11.770, 11.648, 11.833]`
- CFG ms: B `[0.273, 0.264, 0.211, 0.299, 0.228, 0.273, 0.216]`;
  C `[0.242, 0.322, 0.238, 0.262, 0.309, 0.304, 0.267]`

## Structural and allocation evidence

The CFG range types have compile-time size and alignment assertions proving
the same two-`u32` layout as the fields they replace. CFG retains typed value,
call-argument, switch-case, and projection vectors; it does not word-encode or
box those elements. The focused test
`every_payload_family_read_traverses_with_exactly_zero_allocations` constructs
and fully consumes all ten migrated CFG families under a thread-local counting
allocator and observes exactly zero allocations. Atomic builders stage input
before reserving and committing owner storage, and the owner tests exercise
failure without partial publication.
