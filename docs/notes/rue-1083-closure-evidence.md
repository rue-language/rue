# RUE-1083 closure evidence package

> **Status: skeleton.** This package is safe to merge before the final
> measurements. It is not closure evidence until every **OPERATOR FILL** is
> replaced, the referenced raw measurement record is committed, and the
> sign-off block is complete.

This note is the single review surface for closing
[RUE-1083](https://linear.app/steve-klabnik/issue/RUE-1083/restore-large-program-ci-after-semantic-query-cutovers).
It follows the RUE-1092 sign-off shape: state the frozen claim, link every
repair slice, record mechanical evidence without substituting wall time for
structural locality, identify residual ownership, and obtain an adversarial
reviewer verdict.

## 1. Regression statement and original reproduction

The RUE-1026/RUE-1027 declaration/body query cutovers made each reached body
repeat whole-program semantic-context preparation, projection, installation,
and endpoint work. Large-program compile cost therefore grew approximately as
reached bodies times the declaration universe rather than as shared declaration
work plus consumed body facts.

The original required-CI and local reproduction record was:

| Witness | Original result |
|---|---:|
| Caldera, Linux x64, run `29814932021`, job `88583966572` | exceeded the temporary 900 s compiler budget |
| Ordinary macOS CLI, run `29816387037`, job `88588712012` | exceeded the 20 min target budget |
| Meridian at RUE-1027 commit `8aa783d1` | 120.012 s / 120 s timeout |
| Harbor at `8aa783d1` | 30.007 s / 30 s timeout |
| Lattice at `8aa783d1` | 30.010 s / 30 s timeout |
| Mosaic at `8aa783d1` | 30.009 s / 30 s timeout |
| Rill locally/macOS at `8aa783d1` | approximately 15 s |
| Rill, Linux ARM64, run `29826602800`, job `88621258238` | all 13 executions exceeded 30 s |
| Required Valgrind after excluding Caldera, job `88646527867` | more than 28 min in the corpus step |

Instrumentation merged in [PR #1888](https://github.com/rue-language/rue/pull/1888)
localized approximately 85% of large-program wall time to repeated per-body
prepare/project/install work, approximately 3% to real body analysis, and
approximately 11% to query-runtime bookkeeping. Its local release-build
comparison against pre-cutover trunk was:

| Program | Pre-cutover | Reproduction |
|---|---:|---:|
| Rill | 0.32 s | 6.4 s |
| Mosaic | 0.49 s | 14.5 s |
| Harbor | 0.89 s | 65 s |
| Lattice | 1.15 s | 69 s |
| Meridian | 5.7 s | 438 s |

**Closure claim:** the repaired production body path consumes exact provider
facts through a body-local overlay, no longer rebuilds a declaration-wide epoch
per body, restores the deferred real checks and intended budgets, and preserves
the RUE-1026/RUE-1027 single-authority, laziness, and per-family/body query
invariants.

## 2. Repair narrative: merged RUE-1091 slices

This is the complete oldest-to-newest inventory produced by
`git log trunk --oneline --grep=RUE-1091` at skeleton creation. Multiple commits
belong to PR #1904 and PR #1910; they remain separate because the trunk query
enumerates them separately.

- [`5957932f`](https://github.com/rue-language/rue/pull/1904) — drafted the exact body-context repair contract.
- [`42deee86`](https://github.com/rue-language/rue/pull/1904) — completed the exact-provider design contract.
- [`9d014977`](https://github.com/rue-language/rue/pull/1904) — retained lookup roots for deterministic failures.
- [`8b8408ce`](https://github.com/rue-language/rue/pull/1909) — added owned declaration recipes and pure converters (slice 2a).
- [`0d18d54e`](https://github.com/rue-language/rue/pull/1910) — added the body-local semantic overlay and publication conversion (slice 2b).
- [`f378712f`](https://github.com/rue-language/rue/pull/1910) — registered the overlay module in the API inventory and formatted it (slice 2b).
- [`46e6d7f1`](https://github.com/rue-language/rue/pull/1910) — strengthened overlay cancellation coverage after review (slice 2b).
- [`b4be8c22`](https://github.com/rue-language/rue/pull/1914) — made the recipe cache a physical optimization with thrash metering (slice 2c).
- [`ed562cc3`](https://github.com/rue-language/rue/pull/1915) — widened module-index and lookup families to the canonical result contract (slice 3a).
- [`a80c25f8`](https://github.com/rue-language/rue/pull/1916) — added the exact body-fact provider trait and differential adapter (slice 3b).
- [`3253bf0a`](https://github.com/rue-language/rue/pull/1917) — retained the published-root lookup lease in the session and added pressure metering (slice 3c).
- [`636eb0ea`](https://github.com/rue-language/rue/pull/1918) — added overlay-materialization counters (B2, slice 3d).
- [`7d12748c`](https://github.com/rue-language/rue/pull/1919) — recorded the provider-driven analyzer rewire plan and inference-context spike.
- [`3de83771`](https://github.com/rue-language/rue/pull/1921) — changed bare constant names to ordinary scoped resolution (slice r0).
- [`65d8075d`](https://github.com/rue-language/rue/pull/1927) — introduced the `BodyEndpointProvider` seam and `EpochFacts` (slice r1a).
- [`31845b87`](https://github.com/rue-language/rue/pull/1926) — hoisted aggregate resolution behind epoch facts (slice r1c).
- [`46b93d80`](https://github.com/rue-language/rue/pull/1928) — introduced the `CallResolutionFacts` seam and `EpochFacts` (slice r1b).
- [`ea11358f`](https://github.com/rue-language/rue/pull/1929) — added type-syntax `ProviderFacts` and overlay type materialization (slice r2).
- [`d42ce834`](https://github.com/rue-language/rue/pull/1932) — scaffolded the rFinal whole-body differential harness.
- [`8b971030`](https://github.com/rue-language/rue/pull/1934) — made the inference context demand-populated (slice r5b).
- [`1c9f43ea`](https://github.com/rue-language/rue/pull/1931) — added the inert body-analysis capability inventory.
- [`24a75d7f`](https://github.com/rue-language/rue/pull/1935) — added durable parameter names, the comptime-call operation, and `SignatureFacts` (slice r5a).
- [`850f4385`](https://github.com/rue-language/rue/pull/1936) — added the callable-symbol method boundary operation (slice r4a-1).
- [`a4bddde5`](https://github.com/rue-language/rue/pull/1937) — added the nominal/type family to the body identity pool (slice r4a-2a).
- [`7b11ae02`](https://github.com/rue-language/rue/pull/1938) — added the callable family to the body identity pool (slice r4a-2b).
- [`d3210022`](https://github.com/rue-language/rue/pull/1939) — added the RIR-index family to the body identity pool (slice r4a-2c).
- [`1e65db30`](https://github.com/rue-language/rue/pull/1940) — added call-resolution `ProviderFacts` (slice r4b-1).
- [`24b48791`](https://github.com/rue-language/rue/pull/1941) — added endpoint `ProviderFacts` (slice r4b-2).
- [`392583a8`](https://github.com/rue-language/rue/pull/1942) — refined rFinal harness coverage.
- [`7f68217f`](https://github.com/rue-language/rue/pull/1943) — added aggregate `ProviderFacts` and recorded the deferral backlog (slice r4b-3).
- [`0798d23a`](https://github.com/rue-language/rue/pull/1945) — shared the anonymous-identity digest.
- [`90db14e9`](https://github.com/rue-language/rue/pull/1947) — added builtin and slice name facts and lifted bare owners (slice r6a).
- [`6e593b2d`](https://github.com/rue-language/rue/pull/1948) — rethreaded named-method declaration seams.

**OPERATOR FILL — flip and restoration slices:** append every later merged
RUE-1091-tagged commit returned by the same command, one PR-linked line per
commit. Do not replace this inventory with a hand-selected subset.

## 3. RUE-1090 gate verdict: ACTIVATE → CANCEL

### Frozen rule

The gate compares exact deterministic per-body ratios. For totals
`baseline_total`, `grown_total` and cold-body denominators `baseline_bodies`,
`grown_bodies`, the signed slope numerator is:

```text
grown_total × baseline_bodies - baseline_total × grown_bodies
```

Only zero for every projection, installation, and endpoint row in both the
normal and large matrices permits `CANCEL RUE-1091`. Any non-zero slope remains
`ACTIVATE RUE-1091`; wall-time improvement or a lower constant cannot override
it. A historical-witness ceiling failure is a measurement/regression blocker,
not an activation verdict. Timing and allocation instrumentation must be
disabled for the structural run.

### Recorded activation control

The pre-RUE-1112 control on merged trunk `ec99029b` fixed reached bodies at 100
and compared 100 with 1,000 unrelated declarations:

| Gated counter | 100 declarations | 1,000 declarations | Verdict |
|---|---:|---:|---|
| Projection | `20301/101` | `111201/101` | `ACTIVATE RUE-1091` |
| Installation | `20301/101` | `111201/101` | `ACTIVATE RUE-1091` |
| Endpoints | `40703/101` | `222503/101` | `ACTIVATE RUE-1091` |

This was a control, not the post-RUE-1112 decision run. It nevertheless records
the before-repair side of the required transition.

### Post-flip frozen ratio table

Copy exact audit rows from the raw measurement record; do not normalize,
truncate, or replace them with elapsed times.

| Matrix | Counter | Baseline total / cold bodies | Grown total / cold bodies | Exact slope numerator | Row verdict |
|---|---|---:|---:|---:|---|
| Normal: 100 → 1,000 declarations at 100 reached bodies | Projection | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |
| Normal: 100 → 1,000 declarations at 100 reached bodies | Installation | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |
| Normal: 100 → 1,000 declarations at 100 reached bodies | Endpoints | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |
| Large: 100 → 10,000 declarations at 100 reached bodies | Projection | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |
| Large: 100 → 10,000 declarations at 100 reached bodies | Installation | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |
| Large: 100 → 10,000 declarations at 100 reached bodies | Endpoints | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL** | **OPERATOR FILL: `FLAT`** |

| Transition evidence | Value |
|---|---|
| Authoritative post-flip merged-trunk SHA | **OPERATOR FILL** |
| Normal combined verdict | **OPERATOR FILL: `CANCEL RUE-1091 (all gated counters flat)`** |
| Large combined verdict | **OPERATOR FILL: `CANCEL RUE-1091 (all gated counters flat)`** |
| Historical tripwire outcome | **OPERATOR FILL: PASS, with exact emitted rows** |
| Resulting RUE-1090 state and evidence link | **OPERATOR FILL** |
| Resulting RUE-1091 state and evidence link | **OPERATOR FILL** |

## 4. Step-6 measurement ledger

The raw values and complete command output belong in
[`docs/notes/rue-1091-measurement-results.md`](rue-1091-measurement-results.md).
This closure package records their reviewed disposition and links back to that
single raw ledger; it must not duplicate or silently revise the raw results.

| Provenance | Value |
|---|---|
| Measured merged upstream `trunk` SHA | **OPERATOR FILL** |
| Measurement-record commit SHA | **OPERATOR FILL** |
| Operator | **OPERATOR FILL** |
| Adversarial reviewer | **OPERATOR FILL** |
| UTC date/time | **OPERATOR FILL** |
| Host OS and version | **OPERATOR FILL** |
| Architecture / CPU | **OPERATOR FILL** |
| Physical memory | **OPERATOR FILL** |
| Buck2/compiler build configuration | **OPERATOR FILL** |
| Confirmation: counting allocator disabled | **OPERATOR FILL** |
| Confirmation: timing instrumentation disabled for structural runs | **OPERATOR FILL** |
| Working copy clean and based on fetched upstream `trunk` | **OPERATOR FILL** |

Run and record the exact structural commands:

```sh
./buck2 build //crates/rue-compiler:rue-compiler-test
./buck2 run //crates/rue-compiler:rue-compiler-test -- --ignored \
  scaling_matrix_fixed_bodies_growing_declarations --nocapture
RUE_SCALING_LARGE=1 ./buck2 run //crates/rue-compiler:rue-compiler-test -- --ignored \
  scaling_matrix_fixed_bodies_growing_declarations --nocapture
```

| Step-6 row | Verdict / raw-ledger anchor |
|---|---|
| Scaling matrix target build | **OPERATOR FILL** |
| RUE-1090 normal matrix | **OPERATOR FILL** |
| RUE-1090 large matrix | **OPERATOR FILL** |
| RUE-1121 exact-flat context rows | **OPERATOR FILL** |
| RUE-1121 exact invalidation rows | **OPERATOR FILL** |
| Recomputed growing-bodies-axis targets | **OPERATOR FILL** |
| Warm/fresh parity | **OPERATOR FILL** |
| Opposite-order / differential parity | **OPERATOR FILL** |
| Cancellation and forced-eviction coverage | **OPERATOR FILL** |
| `body_analysis_has_no_whole_program_context_path` guard | **OPERATOR FILL** |
| Focused validation and `scripts/rue quick` | **OPERATOR FILL** |

**Step-6 disposition:** **OPERATOR FILL — PASS / BLOCKED**, with a direct
anchor to the raw ledger and a one-sentence explanation for any non-pass row.

## 5. Caldera headline

Caldera is measured separately from structural/allocation instrumentation.

| Measurement | Result |
|---|---:|
| Regression baseline | **DNF — 41 min** |
| Post-flip cold compile, same documented measurement shape | **OPERATOR FILL** |
| Change from baseline | **OPERATOR FILL** |
| 300 s cold compiler budget | **OPERATOR FILL: PASS / FAIL** |

| Caldera provenance | Value |
|---|---|
| Merged upstream `trunk` SHA | **OPERATOR FILL** |
| Exact command and environment | **OPERATOR FILL** |
| Host / architecture | **OPERATOR FILL** |
| Compiler wall time | **OPERATOR FILL** |
| Peak RSS, if measured in a separate invocation | **OPERATOR FILL** |
| Linux x64 required shard | **OPERATOR FILL** |
| Linux ARM64 required shard | **OPERATOR FILL** |
| macOS required shard | **OPERATOR FILL** |
| Real Valgrind corpus restored | **OPERATOR FILL** |
| All temporary RUE-1083 stubs, filters, budgets, and guard workflow removed | **OPERATOR FILL** |

**Headline:** `Caldera: DNF at 41 min before the repair → `**OPERATOR FILL**
` after the provider flip (300 s cold budget: `**OPERATOR FILL**`).`

## 6. Residual follow-ups

Residual work is acceptable only when it is outside the RUE-1083 closure claim,
has a durable tracker, and has a named owner. An unowned carry-forward is a
sign-off blocker, not a residual.

| Follow-up | Why it remains outside this closure | Owner | State at sign-off |
|---|---|---|---|
| [RUE-1128](https://linear.app/steve-klabnik/issue/RUE-1128/record-method-references-at-resolution-time-retire-the-callable-symbol) | Record method references at resolution time and retire callable-symbol reversal. The rewire plan records this as the open “Steve-level question”; the keyed operation is correct under the provider flip, and the AIR/artifact-shape change was deliberately deferred until after it. | Steve (named by the plan); **Linear assignee must be confirmed because it is currently unassigned** | Todo at skeleton creation; **OPERATOR FILL at sign-off** |
| **OPERATOR FILL: additional open carry-forward, or delete this row** | Must cite the exact carry-forward block and explain why it does not weaken structural locality, correctness, CI restoration, or the single-authority cut. | **OPERATOR FILL: named owner** | **OPERATOR FILL** |

Before sign-off, explicitly close or assign and track every still-open obligation
from the plan's review carry-forward blocks: the interner-read decision and
outer-driver guard whitelist; explicit rFinal variant coverage; aggregate
overlay fill-source and keyed receiver join; span-source equivalence; bare-owner
coverage; and provider-era edge truth. Later merged slices may satisfy these
items; cite the satisfying PR/test in the raw ledger rather than carrying stale
plan text forward.

## Sign-off

| Role | Name | Verdict | Evidence / date |
|---|---|---|---|
| Measurement operator | **OPERATOR FILL** | **OPERATOR FILL: PASS** | **OPERATOR FILL** |
| RUE-1092-style adversarial reviewer | **OPERATOR FILL** | **OPERATOR FILL: PASS** | **OPERATOR FILL** |
| CI restoration verifier | **OPERATOR FILL** | **OPERATOR FILL: PASS** | **OPERATOR FILL** |
| Maintainer | **OPERATOR FILL** | **OPERATOR FILL: APPROVE CLOSURE** | **OPERATOR FILL** |

Final reviewer statement:

> **OPERATOR FILL.** I verified the current production source and raw evidence,
> not only the historical plan. The provider/overlay body path is the sole
> supported production authority; the exact structural gate is flat on both
> axes; Caldera and every restored required check meet their stated budgets;
> every residual has a tracker and named owner; and no acceptance criterion is
> deferred by this sign-off.
