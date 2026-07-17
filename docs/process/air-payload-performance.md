# AIR payload-schema performance record

RUE-842 supplies the AIR-specific evidence required by ADR-0056. The later
cross-phase consolidation, clean compiler build, and deterministic owner-crate
incremental rebuild matrix remain RUE-843's integration gate.

## Reproduction and identity

The baseline is commit `50deb00da2e3aa65ae54d037619451c4882d1864`
(tree `f52b0f0ef6619352570c44f6f174de83ace6fb5a`). The baseline profiler was
built from that exact tree plus a measurement-only introspection patch whose
SHA-256 is `b49fda9767a8f9a5a06eb13661cd20111dfbe04fb44e7ba8d01eb3e64a336851`.
That exact patch and the verbatim compiler and micro-profiler `BUCK`/Rust
sources are checked in as
[rue-842-baseline-introspection.patch](perf-data/rue-842-baseline-introspection.patch)
and [rue-842-baseline-profiler](perf-data/rue-842-baseline-profiler/).
The measurements compare that baseline with the complete RUE-842 implementation
before integration. The published change is rebased onto RUE-840 at commit
`cece7dd7331ac711b9f40a98d08ec92771173f15` (tree
`0023b2424f9ea17e92e92b9895e8c6e512443f02`); the rebase only adapted AIR
consumers and tests to RUE-840's validated CFG interface and did not change the
measured AIR implementation or profiler. A subsequent CI hardening fix rejects
oversized array-repeat layouts before expanding their AIR element payload; none
of the three measured compiler workloads contains array-repeat syntax, and the
profiler is unchanged. The exact published RUE-842 delta, including that
fail-fast diagnostic path, is recorded in
[rue-842-source-identity.json](perf-data/rue-842-source-identity.json). That
manifest hashes every file changed from the publication base, including every
profiler, script, document, and raw artifact; only the manifest itself is
excluded to avoid a self-hash. Regenerate it after all other files are final
with:

```bash
python3 scripts/air-payload-fingerprint.py \
  --base cece7dd7331ac711b9f40a98d08ec92771173f15 \
  --output docs/process/perf-data/rue-842-source-identity.json
```

Host: Apple M4, 16 GiB RAM, arm64 macOS 26.5 (Darwin 25.5.0). Toolchain:
Rust 1.92.0 and Buck2 `2026-06-30-c88d791e34884e58617b92d5b98c7f71faee823c`.
Both profiler binaries use the Buck2 release platform
(`//platforms:release`). Their SHA-256 fingerprints are
`6c986b940e2fa24f54926071b00e5f391485f45545ed6a21f3bbd3a0dac2226b`
(baseline) and
`f16b26543be831ab66db1d11c52fadd983b2e27b498a75f04121b7b74e4f2ca7`
(candidate, including its microbenchmark mode).

To deterministically rebuild both baseline binaries, set `EVIDENCE` to this
candidate checkout and start from a clean baseline checkout:

```bash
git checkout 50deb00da2e3aa65ae54d037619451c4882d1864
git apply "$EVIDENCE/docs/process/perf-data/rue-842-baseline-introspection.patch"
mkdir -p crates/rue-air-profile/src crates/rue-air-micro-profile/src
cp "$EVIDENCE/docs/process/perf-data/rue-842-baseline-profiler/compiler.BUCK.txt" \
  crates/rue-air-profile/BUCK
cp "$EVIDENCE/docs/process/perf-data/rue-842-baseline-profiler/compiler-main.rs.txt" \
  crates/rue-air-profile/src/main.rs
cp "$EVIDENCE/docs/process/perf-data/rue-842-baseline-profiler/micro.BUCK.txt" \
  crates/rue-air-micro-profile/BUCK
cp "$EVIDENCE/docs/process/perf-data/rue-842-baseline-profiler/micro-main.rs.txt" \
  crates/rue-air-micro-profile/src/main.rs
./buck2 build --target-platforms //platforms:release \
  //crates/rue-air-profile:rue-air-profile \
  //crates/rue-air-micro-profile:rue-air-micro-profile
```

The output paths printed by Buck2 are the baseline binaries recorded in the
raw JSON. The four verbatim source artifacts have SHA-256 values
`02b10b7489896885c65dc74910cc5328df7486b14aa482009ecf85e24f75a166`
(compiler BUCK),
`b85405b10d194d3a49cf4666a6d5b03f4621386b65b60d087f076dd8fcd70098`
(compiler Rust),
`46fec6356376e17c028d5ec964362d948e2a9e0b015f08e09bc3619fb2a07b9c`
(micro BUCK), and
`b57ece9b012608019aaccccac71b609517cdcaf8e26ea0ebcef7786c5d6156af`
(micro Rust).

The benchmark-only counting allocator is enabled after source publication and
immediately before `CompilerSession::semantic`; it is disabled before storage
accounting and JSON serialization. Each workload runs in a fresh process.
`/usr/bin/time -l` supplies process wall time and peak RSS. One warmup per
revision precedes seven measured baseline/candidate pairs in alternating order.
The checked-in raw arrays, medians, MADs, workload hashes, and storage profiles
are in [rue-842-air-workloads.json](perf-data/rue-842-air-workloads.json), whose
SHA-256 is
`95b5983571e7504c67e41139d9118c9f1ff1749ea23f0ced712ae1a8c698172e`.

Reproduce with:

```bash
python3 scripts/air-payload-workloads.py /tmp/rue-842-workloads
python3 scripts/air-payload-benchmark.py \
  --baseline /path/to/baseline/rue_air_profile \
  --baseline-revision 50deb00da2e3aa65ae54d037619451c4882d1864 \
  --candidate /path/to/candidate/rue_air_profile \
  --candidate-revision 50deb00da2e3aa65ae54d037619451c4882d1864+RUE-842-worktree \
  --output /tmp/rue-842-air-results.json \
  benchmarks/stress/many_functions.rue \
  /tmp/rue-842-workloads/match-heavy.rue \
  /tmp/rue-842-workloads/generic-specialization.rue
```

The checked-in calls workload hash is
`6de992cedb83f6c5f73788574a994d4f74a8a1fa8c45697afffa325f2876e38f`.
The deterministic generator hash is
`a691d369502ca0bfc141a88d05575cf4ac2e85b751f62017be023a0f9ecff66c`;
the generated match and generic inputs have hashes
`4848725ce6c4a86041c558a09e30b6ce7b9c9bd168820a889df391af1db18a6b`
and `df3ae0baea70c421365091bf20636795e2f0e523eeee4561511869845bb8c7b1`.

## Compiler workload result

Values are median ± MAD. Allocation counts are exact calls within the AIR
measurement boundary. The ADR wall/RSS threshold is the larger of 2% or three
times the larger MAD; allocation count may not increase.

| workload | wall seconds baseline → candidate | peak RSS MiB baseline → candidate | AIR ms baseline → candidate | allocations baseline → candidate |
|---|---:|---:|---:|---:|
| 1,001 small functions/calls | 0.04 ± 0.00 → 0.04 ± 0.00 | 28.98 ± 0.45 → 28.80 ± 0.22 | 38.27 ± 0.05 → 39.04 ± 0.11 | 228,948 ± 2 → 227,941 ± 1 |
| 512 match functions with tuple path bindings | 0.03 ± 0.00 → 0.03 ± 0.00 | 24.47 ± 0.11 → 24.58 ± 0.03 | 27.08 ± 0.10 → 27.88 ± 0.07 | 393,183 ± 3 → 390,623 ± 3 |
| 512 comptime/type specialization functions | 0.06 ± 0.00 → 0.06 ± 0.00 | 21.75 ± 0.06 → 21.77 ± 0.05 | 64.86 ± 0.50 → 65.69 ± 0.37 | 476,675 ± 3 → 474,623 ± 1 |

All wall-time and RSS gates pass. Allocation calls decrease by 0.44%, 0.65%,
and 0.43%, respectively. AIR phase medians, reported diagnostically rather
than as an ADR gate, rise by 2.00%, 2.97%, and 1.28%. Requested allocated bytes
rise by 0.60%, 0.15%, and 0.14%; this is
reported separately from retained side-table capacity and remains below the
wall/RSS gate rather than being treated as retained memory.

The original DEFAULT-profile survey exposed a real match allocation regression.
Removing enum-definition clones from finish-time validation, using an inline
visited set for ordinary type graphs, and encoding match arms directly after a
single successful reserve removed it. The release series above is the complete
post-fix rerun.

## Retained and transient storage

Fixed-width families have identical logical bytes in matched workloads. The
calls workload retains 13,608 bytes in both revisions (9,600 call-argument and
4,008 block-statement bytes), with 16,384 bytes capacity. The specialization
workload retains 28,672 bytes with 40,976 bytes capacity in both revisions;
16,384 logical bytes are live call arguments and 12,288 are specialization
words made unreachable by the existing generic-call rewrite in both revisions.

The match workload contains exactly 512 nonempty match payloads. Baseline match
records occupy 24,576 logical bytes; candidate records occupy 26,624 bytes.
The exact 2,048-byte increase is one four-byte count envelope per nonempty
payload, meeting ADR-0056's variable-family structural limit. Total logical
side-table bytes therefore move from 43,016 to 45,064. Total retained capacity
moves from 77,764 to 81,856 bytes; this allocator rounding is reported rather
than assigned fictitiously to individual families sharing the word store.
Candidate match construction has zero scratch bytes after reserve. Baseline
match encoding staged up to 48 bytes in this workload.

## Focused family microbenchmark

The candidate profiler and a measurement-only baseline harness each construct
128 independent 64-element owners per family, then fully consume the selected
range 20,000 times. The baseline harness uses the exact old `Air` representation
and raw builders; the candidate uses the typed builders and borrowing iterators.
Counting is reset around each build/traversal interval, and iterator construction
and every element visit are included. Seven alternating release samples after
one warmup are in
[rue-842-air-microbench.json](perf-data/rue-842-air-microbench.json), whose
SHA-256 is
`84d243c3f99bc5abc5abea68a360c72e1ce0e3368a14859d9cb91add9c020744`.
The baseline micro-profiler binary SHA-256 is
`43b6c68b39ea4eb267e5483304b4fbae61ba3e8b900ca080caecd18c8a8083f8`;
the candidate binary is the profiler identified above. Every traversal in both
revisions performs exactly zero heap allocations.

The checked JSON records both revision strings, absolute binary paths, binary
hashes, all raw samples, and the alternating order of every pair. Reproduce it
with:

```bash
python3 scripts/air-payload-microbench.py \
  --baseline /path/to/baseline/rue_air_micro_profile \
  --baseline-revision 50deb00da2e3aa65ae54d037619451c4882d1864 \
  --candidate /path/to/candidate/rue_air_profile \
  --candidate-revision 50deb00da2e3aa65ae54d037619451c4882d1864+RUE-842-worktree \
  --output /tmp/rue-842-air-microbench.json
```

| family | median M elements/s baseline → candidate | allocations per complete build baseline → candidate | logical/capacity bytes baseline → candidate | peak scratch bytes baseline → candidate |
|---|---:|---:|---:|---:|
| match arms | 3,607 → 304 | 2 → 3 | 1,024 / 1,024 → 1,028 / 1,028 | 1,024 → 0 |
| call arguments | 3,581 → 1,732 | 2 → 3 | 512 / 512 → 512 / 512 | 0 → 0 |
| type arguments | 3,570 → 3,518 | 2 → 2 | 256 / 256 → 256 / 256 | 0 → 0 |
| constant values | 3,692 → 283 | 2 → 4 | 1,280 / 1,280 → 1,280 / 1,280 | 1,280 → 1,280 |
| intrinsic arguments | 3,630 → 3,541 | 2 → 2 | 256 / 256 → 256 / 256 | 0 → 0 |
| block statements | 3,610 → 3,554 | 2 → 2 | 256 / 256 → 256 / 256 | 0 → 0 |
| struct fields | 3,624 → 3,581 | 2 → 4 | 256 / 256 → 512 / 512 | 0 → 0 |
| source order | 3,684 → 3,531 | 2 → 4 | 256 / 256 → 512 / 512 | 0 → 0 |
| array elements | 3,682 → 3,573 | 2 → 2 | 256 / 256 → 256 / 256 | 0 → 0 |
| enum payload | 3,647 → 3,486 | 2 → 2 | 256 / 256 → 256 / 256 | 0 → 0 |
| projections and places | 3,650 → 3,644 | 3 → 3 | 788 / 788 → 788 / 848 | 788 → 768 |

The old traversal numbers black-box raw words or chunks; the typed traversal
numbers materialize the canonical logical elements and therefore include work
that old consumers performed ad hoc. ADR-0056's focused traversal gate is zero
allocations, which both sides meet; whole-compiler wall time, RSS, and allocation
non-regression are measured by the paired workloads above and all pass.

Build counts include the complete caller input plus owner append. Candidate
struct initialization atomically owns equal-length field and source-order
ranges, so each of those rows reports the same paired 512-byte owner while the
baseline harness isolates its individual 256-byte raw slice; the combined
baseline owner is also 512 bytes. The constant stream includes its encoded
scratch vector. Fixed-width direct-element families otherwise introduce no
logical payload bytes relative to the old slices. Opaque ranges remain two
`u32`s by compile-time size/alignment assertions, default accessors return
borrowing iterators, and CFG element stores remain typed direct-element vectors.
The candidate correctness fixture registers matching 64-field and 64-element
enum declarations, a real 64-element array, and nested array identities for all
64 projections, then successfully finishes one owner from every family outside
the timed interval.

## Decision

RUE-842 passes the AIR-specific ADR-0056 correctness, allocation, wall-time,
RSS, storage, staging, envelope, and zero-allocation traversal gates. RUE-843
owns only the explicitly cross-phase clean/incremental build and consolidated
RIR/AIR/CFG report; it does not reopen AIR's payload representation.
