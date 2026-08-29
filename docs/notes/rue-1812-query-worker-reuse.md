# RUE-1812 query-worker construction measurement

This note records the measurement and replacement decision for the physical
workers beneath registered query batches. The scheduler's requested, granted,
and entered-lane counters remain the logical concurrency evidence. Thread
births and coordinator residual are a separate physical observation.

## Measurement contract

`batch_worker_thread_births` counts successful operating-system thread
creations. The donating parent lane is not a birth. In the original scoped
implementation, every granted extra lane created a thread. In the replacement,
one query runtime lazily creates a reusable thread for a newly needed physical
lane, up to `resolved_workers - 1`; later batches reuse those workers.

`batch_worker_coordinator_residual_ns` is coordinator latency with useful
worker execution excluded. Before the replacement it was synchronous scoped
thread creation plus only the join tail after the worker recorded completion.
After the replacement it is runtime worker creation, synchronous job dispatch,
plus only the completion-delivery tail after the worker recorded completion.
Time blocked waiting for a worker to finish is excluded in both cases. Because
creation or dispatch can overlap already-running worker execution, the residual
is not additive with query-worker active time, request wall time, or compiler
root time.

The count uses no worker-entry contention: the submitting batch coordinator
counts successful lazy construction locally and publishes one relaxed atomic
update when the batch coordinator scope exits, including during unwind. The
residual follows the same one-publication-per-batch shape.

## Method

Both sides were measured on 2026-08-29 from the same final RUE-1812 tree based
on commit `fa4f2730cbe01d1632376cab0fde52e62c1ba974`. The checked-in
`performance/rue-1812-scoped-thread-baseline.patch` transforms that tree to the
scoped per-batch mapping while preserving the final counter names and wire
schema. Its SHA-256 is
`38cb3013f29cb271e6c6b70ffcfe5d66c58dd62e447c13b79f5d9274e0ec7ee1`.
The patch also removes only the two reusable-capacity validation checks which
the counterfactual necessarily violates; all other boundary and incremental
validation remains active. Apply and restore it exactly with:

```text
git apply --check performance/rue-1812-scoped-thread-baseline.patch
git apply performance/rue-1812-scoped-thread-baseline.patch
# build and collect the baseline reports
git apply --reverse --check performance/rue-1812-scoped-thread-baseline.patch
git apply --reverse performance/rue-1812-scoped-thread-baseline.patch
# rebuild and collect the final reports
```

The compiler revision field names the common base because neither physical
mapping was committed while these reports were collected.

The host was a MacBook Pro `Mac17,2`, Apple M5, 10 cores (4 performance and 6
efficiency), 24 GB memory, macOS 26.5.2 build 25F84. The compiler target was
`x86-64-linux`. The benchmark fingerprint recorded `local/local`, 10 cores;
local CPU and memory probes were unavailable to the sandbox, so the manual
hardware fingerprint above completes the record. The runner executed samples
sequentially. OS page-cache state was uncontrolled.

The optimized products were built and located with the canonical commands:

```text
./buck2 build //crates/rue:rue //crates/rue-bench:rue-bench --target-platforms //platforms:release
scripts/rue-bin --target-platforms //platforms:release
./buck2 build //crates/rue-bench:rue-bench --target-platforms //platforms:release --show-simple-output
```

Cold compilation used `performance/rue-1812-scaling.toml`, SHA-256
`64235b5a6becc4dceee0a3e12ffe61b1c9924ef35a173fb2042d01f0e6d94fe8`.
It contains only Mosaic and Lattice, retaining revision 4, release thin LTO,
the `fresh_source_to_native_v1` boundary, three samples, and the complete
`one/two/four/eight/automatic` worker matrix:

```text
LABEL=baseline # use final after restoring and rebuilding the reusable mapping
rue-bench scaling --manifest performance/rue-1812-scaling.toml \
  --compiler "$(scripts/rue-bin --target-platforms //platforms:release)" \
  --commit fa4f2730cbe01d1632376cab0fde52e62c1ba974 \
  --repo-root "$CHECKOUT" --out "/tmp/rue-1812-scaling-repro-${LABEL}.json"
```

Warm edits used the checked-in retained-session corpus without reducing its
coverage: all eight Mosaic and Lattice scenarios, five independent samples for
the reached-body timing row, one structural sample for every other row, and the
1,000-revision retention witness.

```text
LABEL=baseline # use final after restoring and rebuilding the reusable mapping
rue-bench incremental --manifest performance/incremental.toml \
  --fixtures performance/incremental-fixtures.toml \
  --commit fa4f2730cbe01d1632376cab0fde52e62c1ba974 \
  --repo-root "$CHECKOUT" --out "/tmp/rue-1812-incremental-repro-${LABEL}.json"
```

Raw JSON SHA-256 identities were:

| observation | before | after |
| --- | --- | --- |
| cold scaling | `6db94accfb476d0db310c65f2a3e79ed89eb2e80ffb6bc51c8f1f537802573fa` | `c97f1a5a4787b0d4026fc005e895365f23a5dc44984e7d7286ce0c29886ff178` |
| retained edits | `42842175230913c52c74b3b6be1b5824c1d5d7daf013bbc70e8062f473f8d95b` | `754f47bddecbe81b3830ff48c17b3a180c3e3c43ec4c2a29ad2ede44360865a7` |

## Cold results

Values are medians of three fresh processes. Compiler and residual columns are
milliseconds; RSS is MiB. `automatic` resolved to 10 workers.

| workload / workers | OS births before → after | coordinator residual before → after | compiler root before → after | peak RSS before → after |
| --- | ---: | ---: | ---: | ---: |
| Mosaic / 1 | 0 → 0 | 0.000 → 0.000 | 354.23 → 463.99 | 186.6 → 188.6 |
| Mosaic / 2 | 1,115 → 1 | 18.454 → 3.597 | 434.13 → 480.39 | 191.9 → 194.5 |
| Mosaic / 4 | 1,591 → 3 | 20.521 → 5.283 | 379.97 → 453.78 | 192.9 → 194.2 |
| Mosaic / 8 | 1,928 → 7 | 23.775 → 6.234 | 376.12 → 422.35 | 193.8 → 194.5 |
| Mosaic / automatic | 2,031 → 9 | 24.404 → 6.253 | 367.23 → 416.50 | 194.0 → 196.7 |
| Lattice / 1 | 0 → 0 | 0.000 → 0.000 | 833.74 → 835.02 | 399.0 → 424.6 |
| Lattice / 2 | 2,425 → 1 | 42.918 → 6.179 | 1,199.80 → 1,173.64 | 413.8 → 416.8 |
| Lattice / 4 | 3,276 → 3 | 41.639 → 8.027 | 1,106.91 → 1,071.67 | 413.3 → 417.6 |
| Lattice / 8 | 3,922 → 7 | 50.403 → 8.755 | 1,158.75 → 1,128.79 | 413.2 → 418.2 |
| Lattice / automatic | 4,125 → 9 | 57.568 → 8.817 | 1,163.86 → 1,129.20 | 413.5 → 417.8 |

The construction residual fell 73.8–85.6% across parallel rows. Compiler-root
wall time moved in both directions, as did the unchanged single-worker control,
so this host-noisy three-sample run supports no causal wall-time estimate.
Parallel peak RSS increased by 0.7–2.7 MiB for Mosaic and 3.0–5.0 MiB for
Lattice. The single-worker Lattice control also moved by 25.6 MiB, so this
uncontrolled host run does not attribute those shifts to reusable workers.

The compiler exercised semantic, CFG, codegen, object, and linking batches in
these complete source-to-native runs. The full requested/granted/entered
evidence remains in the raw reports; only physical births collapsed to runtime
capacity.

## Retained-edit results

These values are per warm request. Residual is median milliseconds (MAD is in
the raw report). Every after row recorded zero thread births because all
physical workers were created by the retained runtime before the measured
request.

| workload / scenario | births before → after | coordinator residual before → after |
| --- | ---: | ---: |
| Mosaic / no-op re-observation | 54 → 0 | 1.201 → 0.062 |
| Mosaic / unreachable body | 54 → 0 | 0.786 → 0.075 |
| Mosaic / reached body | 716 → 0 | 8.587 → 1.356 |
| Mosaic / callable signature | 755 → 0 | 9.968 → 1.432 |
| Mosaic / layout/ABI | 718 → 0 | 9.099 → 1.226 |
| Mosaic / import set | 753 → 0 | 11.348 → 1.148 |
| Mosaic / reachability deletion | 750 → 0 | 9.247 → 1.384 |
| Mosaic / error introduction | 1,342 → 0 | 18.036 → 2.521 |
| Lattice / no-op re-observation | 54 → 0 | 0.889 → 0.075 |
| Lattice / unreachable body | 54 → 0 | 0.939 → 0.072 |
| Lattice / reached body | 1,266 → 0 | 25.377 → 1.975 |
| Lattice / callable signature | 1,305 → 0 | 24.336 → 2.138 |
| Lattice / layout/ABI | 1,268 → 0 | 22.783 → 2.145 |
| Lattice / import set | 1,305 → 0 | 27.949 → 2.628 |
| Lattice / reachability deletion | 1,300 → 0 | 23.577 → 2.407 |
| Lattice / error introduction | 2,442 → 0 | 38.975 → 4.014 |

Residual fell 84.2–94.8%. The five-sample reached-body runnable median moved
from 96.692 to 92.539 ms for Mosaic and from 325.202 to 224.341 ms for Lattice.
Single-sample scenario wall times remain host-noisy and are not used to claim a
per-scenario speedup. The complete incremental collection moved from 32.260 to
25.671 seconds.

## Correctness and decision

The checked-in `performance/rue-1812-determinism-projection.jq` defines both
determinism projections. The cold projection contains workload, worker setting,
resolved workers, every native-output SHA-256, and the complete one-worker work
record. The warm projection contains workload, scenario, worker mode, sample
index, outcome kind and diagnostic/warning/executable digests, plus every
recursively selected `computed` and `reused` field. They are reproduced with:

```text
jq -cS --arg projection cold \
  -f performance/rue-1812-determinism-projection.jq REPORT.json | shasum -a 256
jq -cS --arg projection warm \
  -f performance/rue-1812-determinism-projection.jq REPORT.json | shasum -a 256
```

The cold baseline/final projections were byte-identical, SHA-256
`9f8f7a11db0d8958434629cf081b8de33831c973e8ec8ff6bd10b0a80b33056b`.
Every warm result matched its independently compiled fresh oracle before and
after. The warm baseline/final projections were also byte-identical, SHA-256
`8e0764fae27107db59b63f04d38ca42744149ee19d90b818953d0504afc10e44`.
The 1,000-revision witness retained the same 45,839 query evictions, 5,056,985
byte peak, and 5,040,173 byte final footprint.

The observed, non-additive residual/root ratios were roughly 3.6–6.6% for cold
parallel rows. Those ratios are neither exclusive time shares nor causal
wall-time estimates: dispatch and completion delivery can overlap useful work.
The replacement is justified by
the structural 1,115–4,125 cold thread births, the 73.8–85.6% cold and
84.2–94.8% warm residual collapse, the warm endpoint evidence, and the host
resource risk of repeated 8 MiB thread creation. It is query-runtime-owned, not
compiler-owned. `BatchWorkerClaim`, the shared permit budget, parent permit
donation, and the wait graph remain the only admission/concurrency authority;
the reusable executor only maps already-granted slots to physical workers.

Each batch still submits a bounded set, executes one lane inline, waits for all
submitted jobs, absorbs results in stable item order, and only then releases
structured waits and the donated permit. Job panics are caught on the reusable
worker, delivered to the coordinator, and resumed after all sibling jobs are
collected. An unconsumed job handle also joins in `Drop`, so unwinding partway
through lazy submission waits for every already-queued job before batch guards,
wait edges, or the worker claim can unwind. Cancellation, cycles, retention
handoff, nested-batch saturation, diagnostics/work determinism, and one-worker
inline execution therefore keep their existing structured semantics. Runtime
teardown closes the queue and joins every reusable worker.

An eager first cut charged every runtime for its full physical capacity and
failed local broad validation when concurrent corpus work exhausted the host's
thread budget. The final executor creates a worker only when the existing
scheduler admits a lane that needs one. Final quick and premerge tiers passed.
The standard tier progressed past the executor failure but the heavily loaded
host then refused the oracle's pre-existing 256 MiB interpreter-worker spawn;
an isolated retry failed at the same oracle site. CI remains the full-corpus
authority for that host-resource-limited lane.

The runtime creates workers lazily rather than charging every short-lived
runtime for its full capacity. Each worker retains the established 8 MiB stack
reservation until runtime teardown. Parallel RSS shifts were at most 5.0 MiB,
while the 25.6 MiB shift in the unchanged single-worker control prevents causal
attribution on this host. `-j1` creates no thread and has no executor
coordinator residual.
