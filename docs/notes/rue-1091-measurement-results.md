# RUE-1091 post-flip measurement results

Use this template for the authoritative run after the analyzer flip. The runner
retains a `summary.tsv` plus one full log per stage:

```sh
scripts/rue-1091-measurement.sh --results-dir /absolute/path/to/results
```

For plumbing validation on current trunk, use `--pre-flip`. That mode changes
only the Caldera stage to `--allow-fail`; it does not relax any structural or
correctness stage. Resume an interrupted run with `--stage N` and the same
results directory.

## Provenance

- Commit:
- Date/time:
- Host and OS:
- CPU / logical processors:
- Memory:
- Build configuration: Buck2 release platform for measurement stages
- Results directory:
- Mode: post-flip authoritative / pre-flip plumbing
- Caldera budget: 300 seconds (authoritative runs must leave
  `RUE_1091_CALDERA_BUDGET_SECONDS` unset)

Timing and memory in stage 8 come from the ordinary allocator binary. Allocation
counts in stage 7 come from the distinct counting-allocator binary. Do not merge
or compare their orchestration times from `summary.tsv`; those values include
build and harness overhead and are operational only. The R5 evidence is the
compiler timing reported by stage 8 only.

## Stage ledger

| Stage | Metric | Baseline reference | Post-flip value | Verdict |
| ---: | --- | --- | --- | --- |
| 1 | RUE-1090 normal matrices: fixed bodies / 100→1,000 declarations; fixed declarations / 100→1,000 bodies; identity invariance | Frozen ratios derived from the checked-in harness formulas and reproduced by the pre-cut control run: projection `20301/101 → 111201/101`; install `20301/101 → 111201/101`; endpoints `40703/101 → 222503/101`. The growing-bodies rows must remain linear in body count; identity rows must be invariant. Cancellation requires exact ratio equality (zero slope). | TBD | TBD |
| 2 | RUE-1090 large declaration matrix: stage-1 fixed-body rows plus 10,000 unrelated declarations | Same frozen 100-declaration denominators as stage 1. Historical whole-universe 10k-declaration witnesses derived from the checked-in harness formulas: projection/install `1020201/101`; endpoints `2040503/101`. These witnesses are informational; the verdict still requires exact flatness. | TBD | TBD |
| 3 | RUE-1121 exact-flat cold rows and edit invalidation: unrelated declaration, body-only edit, exact declaration consumers, negative→positive lookup | Baseline is the checked-in RUE-1121 target/witness envelope. Post-flip target: exact-flat context rows; recompute sets `∅`, `{b0}`, `{left,right}`, and `{extra,main}` respectively, with warm/fresh parity. | TBD | TBD |
| 4 | Differential warm/fresh oracle corpus: artifacts, diagnostics, references, identities, failures, recovery, and bounded eviction | Current-trunk `//crates/rue-compiler:rue-compiler-differential-oracle-test` passes. | TBD | TBD |
| 5 | Forced-eviction and cancellation suites | Current-trunk compiler cancellation and eviction unit suites pass; no canceled/stale value may commit or survive eviction incorrectly. | TBD | TBD |
| 6 | Whole-body transaction equality across schedule permutations, including the forced-eviction/cancellation corpus shape | Current-trunk rFinal side-A self-equality schedule oracle passes. | TBD | TBD |
| 7 | Cold and warm-edit allocation calls/bytes at 1,000 bodies / 100 unrelated declarations | Capture a paired pre-flip counting-allocator run if comparison is required; no frozen numeric allocation baseline exists today. | TBD | TBD |
| 8 | Ordinary-allocator cold/warm semantic and pre-link timing plus cold peak RSS at 1,000 bodies / 100 unrelated declarations | R5 comparison baseline: paired pre-flip stage-8 log. The historical ~45 ms Caldera pre-link figure is an eventual reference-host target, not this synthetic-corpus gate. | TBD | TBD |
| 9 | Caldera cold release compile wall time and `--time-passes`; hard post-flip budget ≤300 s | RUE-1090 activation evidence, with RUE-1083 history: the session-local `rue-caldera-post1112-baseline/` artifact recorded a DNF after approximately 41 minutes. Operator must confirm that baseline provenance at run time. Post-flip acceptance budget: 300 seconds. | TBD | TBD |

## RUE-1090 raw audit rows

Copy the stage 1 and stage 2 audit lines verbatim, including totals, cold-body
denominators, exact signed slope numerators, historical-tripwire results, and
combined verdicts.

```text
TBD
```

## Notes and anomalies

- Record retries, host contention, unavailable peak-RSS support, and any
  `ALLOW_FAIL` result here.
- A non-flat RUE-1090 ratio is a failure even if wall time improves.
- A historical-witness ceiling failure is a measurement blocker, not an
  ordinary activation result.
- The authoritative post-flip run must not contain `ALLOW_FAIL`.
