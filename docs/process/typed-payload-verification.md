# Typed IR payload verification and performance matrix (RUE-843)

This is the cross-phase evidence for the RIR, AIR, and CFG migrations in
[ADR-0056](../designs/0056-typed-ir-payload-schemas.md). Phase-local records
remain authoritative for their migrations; this page verifies the integrated
production path and the measurements they deferred.

## Reproduction identity

- Migration baseline: exact upstream `e71cdbe212cd7f8a8d376c605468016bedb30d61`.
  It includes the stabilized inline-type-constructor path used by the measured
  candidate, so the pair isolates the RUE-843 delta.
- Integrated measured source: exact commit
  `178c4f48ab50c3444d34ce149299ad82e3c1857a`.
- Host: Apple ARM64 T8132, macOS 26.5.2 (25F84), Darwin 25.5.0.
- Toolchain: Rust 1.92.0; Buck2
  `2026-06-30-c88d791e34884e58617b92d5b98c7f71faee823c`.
- Workload profiler: `//crates/rue-air-profile:rue-air-profile` on
  `//platforms:release`. Counting surrounds the production
  `CompilerSession::semantic` query, which publishes validated RIR and AIR and
  builds and validates every CFG.

Exact alternating samples, binary hashes, workload hashes, profiles, medians,
and MADs are checked in at
[rue-843-payload-workloads.json](perf-data/rue-843-payload-workloads.json).
Reproduce them with:

```text
python3 scripts/air-payload-workloads.py /tmp/rue-843-workloads
python3 scripts/air-payload-benchmark.py \
  --baseline /path/to/baseline/rue_air_profile \
  --baseline-revision e71cdbe212cd7f8a8d376c605468016bedb30d61 \
  --candidate /path/to/candidate/rue_air_profile \
  --candidate-revision 178c4f48ab50c3444d34ce149299ad82e3c1857a \
  --output /tmp/rue-843-payload-workloads.json \
  benchmarks/stress/many_functions.rue \
  /tmp/rue-843-workloads/match-heavy.rue \
  /tmp/rue-843-workloads/generic-specialization.rue
```

The publication delta after the measured source is evidence-only: this report
and the three generated JSON records. Their SHA-256 fingerprints are
`2d3b6fb89192b90a4e348df6a2efe75b25fc9d995f3a437dddc5f41dda054061`
(workloads),
`79ac14ac1d136630d5b860f42c8660a8c4fdc109bdae3a5a18d3f000aa9b2aee`
(family matrix), and
`420ca2e7ebb5a7c3716e6e18e5807e23c070c1a5c5c6a2d1995073b4cbe368c3`
(builds). No Rust, Buck, or benchmark-script source differs from measured
commit `178c4f48`.

## Shared verification pattern

Each owner exports a stable family-name inventory: 17 RIR families, 10 AIR
families, and 10 CFG families. The external
`//crates/rue-compiler:rue-compiler-payload-schema-test` repeats those expected
lists deliberately, so a new family fails CI until cross-phase coverage is
reviewed. It enters only through `CompilerSession`; representative aggregate,
array, enum, match, call, intrinsic, block, branch, loop, and projection source
is lowered through every production validator. Published CFGs are cloned and
displayed twice and must remain byte-for-byte stable. A fixed LCG generates 32
bounded arithmetic/branch programs, and cold repeated publication must produce
identical validated displays.

Malformed fixtures remain inside the owner modules, where private metadata can
be corrupted without `unsafe` or raw public constructors. They cover canonical
empty ranges, partial records, overflowing ends, invalid tags and scalars,
trailing words, invalid references, wrong projection kinds, and ranges detached
from another owner. Errors identify the phase, family, range, record, and exact
expected/available width where applicable.

The `payload_schemas` fuzz target uses safe owner-local decoder hooks plus the
production compiler-session path. Those hooks are compiled through dedicated
fuzz-support Buck targets and are absent from the production RIR, AIR, and CFG
crate surfaces.

## Whole-compiler result

One warmup preceded seven fresh-process pairs with alternating pair order. The
gate permits candidate minus baseline up to the larger of 2% of baseline or
three times the larger MAD; allocation calls may not increase.

| Workload | Wall | Peak RSS | Semantic query | Allocation calls | Requested bytes | Result |
|---|---:|---:|---:|---:|---:|---:|
| 1,001 functions/calls | 0.00% | -0.37% | -1.06% | -2.07% | -0.21% | pass |
| 512 tuple-binding matches | 0.00% | +1.02% | -0.02% | -1.08% | -0.27% | pass |
| 512 generic specializations | 0.00% | +0.43% | -0.06% | -0.77% | -0.04% | pass |

Every gated runtime metric passes. Requested bytes are reported separately from
retained capacity; allocation calls fall on every workload.

## Storage, allocation, and staging evidence

The representative candidate profiles are:

| Workload | RIR logical / capacity | RIR peak staging / envelopes | AIR logical / capacity | AIR peak staging / envelopes | CFG logical stores | CFG peak staging |
|---|---:|---:|---:|---:|---:|---:|
| functions/calls | 47,208 / 57,344 B | 0 B / 0 | 13,608 / 16,384 B | 0 B / 0 | values 2,136 B; calls 9,600 B | 0 B |
| tuple-binding matches | 88,116 / 176,128 B | 24 B / 513 | 45,064 / 81,856 B | 0 B / 512 | values 6,152 B; calls 4,096 B; switches 16,384 B | 0 B |
| generic specialization | 34,928 / 57,344 B | 0 B / 0 | 28,672 / 40,976 B | 0 B / 0 | calls 16,384 B | 0 B |

Capacity is reported once per shared store and is not assigned fictitiously to
individual families. Exact atomic-builder staging closes RUE-841's deferred
measurement.

The seven-sample per-family matrix is checked in at
[rue-843-family-microbench.json](perf-data/rue-843-family-microbench.json).
All 37 families build and fully traverse through typed APIs, and every traversal
performs zero heap allocations. Twelve of 17 RIR families, nine of 10 AIR
families, and all 10 CFG families have zero staging. The remaining RIR
variable-width records and AIR constant values retain bounded atomic staging.
CFG value, call-argument, switch-case, and projection stores remain direct typed
vectors rather than word encoding or boxing.

## Compiler build result

The authoritative five-pair true-clean series is checked in at
[rue-843-builds-final.json](perf-data/rue-843-builds-final.json). Each owner
probe appends a fixed comment, builds `//crates/rue:rue`, restores the original
bytes, and performs an untimed settling build. The script records source hashes,
raw wall/RSS/action samples, medians, and MADs.

| Build | Baseline to candidate median | Actions | Gate |
|---|---:|---:|---:|
| clean | 50.67 s to 49.84 s | 364 to 364 | pass |
| RIR incremental | 6.84 s to 7.06 s | 16 to 16 | pass within MAD |
| AIR incremental | 6.18 s to 6.21 s | 12 to 12 | pass |
| CFG incremental | 4.38 s to 4.43 s | 8 to 8 | pass within MAD |

Peak RSS passes for every build, and every action count is exactly unchanged.
Clean and AIR medians improve or remain within the ordinary 2% gate. The RIR
and CFG deltas remain below three times the larger paired MAD. The
AIR-specific exception is therefore not needed by this exact-source series;
all ADR-0056 performance acceptance items pass under the ordinary gate.

Earlier diagnostic series remain checked in to preserve the optimization trail:
[initial](perf-data/rue-843-builds.json),
[mandated rerun](perf-data/rue-843-builds-rerun.json), and
[post-optimization](perf-data/rue-843-builds-post-optimization.json).

## Verification commands

```text
scripts/rue fmt
./buck2 test //crates/rue-rir:rue-rir-test \
  //crates/rue-air:rue-air-test \
  //crates/rue-cfg:rue-cfg-test \
  //crates/rue-compiler:rue-compiler-payload-schema-test \
  //crates/rue-fuzz:rue-fuzz-test
./buck2 build //crates/rue:rue
./buck2 run //crates/rue-fuzz:rue-fuzz -- \
  payload_schemas benchmarks/stress --max-runs=100
scripts/rue quick
scripts/rue test
```
