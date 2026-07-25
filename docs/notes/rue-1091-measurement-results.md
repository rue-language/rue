# RUE-1091 measurement ledger

This file contains two deliberately separate evidence tracks. They must not
be merged into one "before/after" claim.

## Current-production value audit

The reproducible user-value audit is run by
[`scripts/rue-value-audit.py`](../../scripts/rue-value-audit.py), using the
fixture matrix in [`benchmarks/value-audit/manifest.toml`](../../benchmarks/value-audit/manifest.toml).
Its candidate baseline is the current production compiler. A historical
pre-query revision is retained only as context; if it cannot consume the
modern benchmark/session protocol, the runner uses a thin cold black-box
compile fallback. Cross-protocol timing comparisons are indeterminate and
missing required warm evidence is unsupported—not silently treated as zero
work. A same-binary run is a protocol smoke only, not historical comparison
evidence. It is incorrect to call that historical result post-flip evidence or
to backport the modern incrementality harness solely to make the comparison
complete.

The value-audit gates are exact correctness/locality inputs, warm unrelated
edit improvement of at least 50%, warm isolated body-edit improvement of at
least 25%, claimed wins exceeding three times the larger MAD, cold wall/RSS
regression no greater than `max(2%, 3 * larger MAD)`, repeated-edit RSS
stabilization within a 5% band when a persistent-session protocol exists, and
and per-workload absolute cold wall budgets. Caldera keeps its 300-second
budget; `rill`, `mosaic`, `harbor`, `lattice`, and `meridian` carry budgets
derived from the pre-cutover figures recorded in
[`rue-1083-closure-evidence.md`](rue-1083-closure-evidence.md). Each workload's
process timeout sits above its own budget so an over-budget compile is recorded
as a gate failure rather than raising out of the run. Those budgets are checked
per role without pairing, so they return a verdict even when no historical
baseline binary exists and every pair is unsupported. When a distinct historical
baseline IS supplied the absolute budget becomes advisory and the
host-independent pair comparison decides the scenario, because an absolute
budget calibrated on another host can otherwise fail a repaired compiler; see
[`pre-cutover-baseline-binary.md`](pre-cutover-baseline-binary.md). Cold RSS is part of the
cold pair verdict, not a detached annotation. The report records medians, MADs,
explicit role provenance, hashes, host provenance, raw alternating samples, and
unsupported/indeterminate cases.

The pre-cutover figures are transcribed prose evidence from a local
release-build comparison, not a run of this protocol. They give the absolute
gates a checked-in reference; they are never reported as a measured role row,
and the runner rejects a manifest that claims otherwise.

This is the current-production value audit, not the activation ledger below.

## Executed probes

RUE-1091's ordered probe #1 — clone-from-template, feasibility measurement only
— has been run. Its result and the explicit read it produces on whether an
epoch-sharing repair can reach pre-cutover parity are in
[`rue-1133-clone-from-template-probe.md`](rue-1133-clone-from-template-probe.md),
with raw evidence in
[`../benchmarks/rue-1133-clone-from-template-probe.jsonl`](../benchmarks/rue-1133-clone-from-template-probe.jsonl).
It is neither a value-audit row nor activation evidence: a cheap copy narrows no
dependency and cannot satisfy any RUE-1091 acceptance criterion.

## Landed repairs

RUE-1135 (A3) built the request-scoped immutable declaration base the probe was
run to size: one bound declaration epoch per rooted attempt, with each reached
body deriving its epoch from it instead of rebuilding one. Design, the
soundness argument, and a cold-coefficient measurement are in
[`rue-1135-request-scoped-declaration-base.md`](rue-1135-request-scoped-declaration-base.md),
with raw evidence in
[`../benchmarks/rue-1135-declaration-base.jsonl`](../benchmarks/rue-1135-declaration-base.jsonl).

It is a real coefficient win (3.6× to 8.4× cold semantic phase on the synthetic
curve, growing with the declaration universe) and it is not activation evidence
either. It narrows no dependency: an unrelated declaration edit still
invalidates the base and therefore every body, and cold work remains
`O(bodies × declarations)` with a smaller coefficient. RUE-1091's completion
criterion and RUE-1093's gate stay open; closing them additionally requires
RUE-1134 (B1) and the provider path.

## Future post-flip activation evidence

The staged gauntlet and result template below remain the authoritative plan for
the future post-flip run. Stage 8/9 values must not be filled with a
current-production value-audit row, and a pre-flip `ALLOW_FAIL` is only
plumbing validation for that future gauntlet.

### What a green gauntlet does and does not establish

Read this before filling stage 3, 8, or 9, because the gauntlet's warm rows
cannot observe one known cost and a clean sweep will otherwise be over-read.

Retained work is validated across revisions by a compatibility token. An
ordinary `session.update` publishes a constant token; import discovery publishes
the import-request generation, which every `begin_import_input_request`
increments. A request that goes through discovery therefore cannot validate
anything it retained. See
[`module-axis-locality-findings.md`](module-axis-locality-findings.md) for the
measurement and the mechanism.

Every locality-asserting row in this gauntlet runs on the constant-token path:

- the RUE-1121 exact-recompute-set and exact-flat context rows (stage 3) drive
  `session.update` plus `canonical_semantic` with no discovery at all;
- the stage 7/8 allocation and timing rows use `rue-scaling-bench`, which does
  the same;
- stage 9 is a cold Caldera compile, where warm reuse does not apply.

The one harness row that does drive the rooted-demand protocol
(`correctness_oracle_import_edit_compares_imported_body_and_linked_bytes`)
asserts warm/fresh parity and that the edited value's consumer refreshed. It
asserts no locality, which is why it passes today.

Two consequences:

1. **No stage is gated on the epoch-reset decision.** The flip does not need to
   wait for it, and a stage that fails must be explained by the flip rather than
   by this.
2. **A green sweep is not evidence that a driver-shaped warm rebuild is fast.**
   It measures an in-process host that manages its own snapshots. Extending any
   stage-8 conclusion to `rue main.rue` on a developer's machine is unsupported
   by this gauntlet as written.

If the project wants to claim (2), the gauntlet needs one added row that drives
a warm edit through the same rooted-demand protocol the driver uses. Until that
row exists, the post-flip summary should say which host shape it measured.

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

- Record retries, host contention, unavailable peak-RSS support, protocol
  fallbacks, and any `ALLOW_FAIL` result here. The value-audit session schema
  currently declares exact recompute/reuse identity sets unavailable; no
  provider identity evidence is invented.
- A non-flat RUE-1090 ratio is a failure even if wall time improves.
- A historical-witness ceiling failure is a measurement blocker, not an
  ordinary activation result.
- The authoritative post-flip run must not contain `ALLOW_FAIL`.
