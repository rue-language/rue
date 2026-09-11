# Daemon resource qualification

Status: qualification record, 2026-09-11. RUE-2129 qualifies the opt-in
local compiler daemon's ownership, bounded transport, cancellation, and
retention behavior. It does not enable automatic daemon use; the separate
performance and rollout decision remains with RUE-2130.

## Policy under test

The release candidate uses one retained host and four automatic compiler
workers. Its service policy is explicit and independent of ordinary
`CompilerSessionConfig` defaults:

| Resource | Provisional limit or behavior |
| --- | --- |
| accepted connections | 32 build/control connections plus one reserved control slot |
| queued requests | 8, with one active compile |
| retained query charge | 256 MiB |
| dependency pins | 1,000,000 |
| response leases | 64 MiB aggregate soft pressure target |
| control reads and response writes | 5 seconds each, as whole-operation deadlines |
| idle lifetime | 30 minutes in the service policy |

The 64 MiB response value is an aggregate pressure policy. One valid answer
may exceed it as a single protected soft overflow; the owner waits for that
answer's lease before producing another response. The serialized result frame
still has its existing hard protocol bound, and transfer buffers are charged
until they are written or dropped. Within the existing hard serialized-frame
bound, pressure never replaces a valid completed answer with a failure or
truncates an artifact.

`daemon status` reports current connections, queue, hosts, response bytes,
retained query charge, dependency pins, source snapshot file/byte counts, and
peak response/retained-charge/pin values. Source bytes are the lengths of the
accepted `SourceSnapshot` texts and can overlap query charge; they are not a
total-heap or RSS measurement. Query-charge and pin peaks are sampled after a
request completes and before the owner applies idle-host trimming; the response
peak is recorded when its lease is acquired. Eviction does not erase the
evidence. RSS remains an external calibration observation rather than a status
field.

## Correctness and ownership evidence

The service implementation keeps one compiler owner, one bounded FIFO, and
one RAII response lease from the bounded serialization result through channel
transfer, socket writes, and drop. Listener admission acquires the connection
lease before spawning a handler. Control and transfer loops enforce
absolute deadlines even when a peer makes continuous partial progress. Stop
cancels active work, closes admission, and joins the bounded handler set.

The permanent service tests in
[`tests.rs`](../../crates/rue/src/daemon/tests.rs) and
[`service.rs`](../../crates/rue/src/daemon/service.rs) cover
connection and queue admission, stale endpoints, identity refusal, stop and
idle retirement, response leasing, owner backpressure, pressure eviction,
slow-drip control frames, non-reading transfers, partial watcher frames,
disconnect cancellation, and crash records. In particular:

- `owner_eviction_preserves_the_answer_and_reports_pretrim_peaks` proves a
  valid answer survives post-request trim and that pre-trim peaks remain
  visible while current gauges return to zero.
- `a_held_response_blocks_the_next_build_until_it_is_drained` proves a held
  completed answer prevents a second completed answer from accumulating, then
  allows the queued request to proceed after drain.
- `a_partial_watcher_frame_does_not_hold_completion_past_its_deadline`,
  `a_slow_drip_control_frame_has_one_absolute_deadline`, and
  `a_nonreading_peer_cannot_hold_a_chunk_transfer_past_its_deadline` cover
  partial reads, slow reads, and slow writes under absolute deadlines.
- `accepted_connections_have_a_hard_bound_and_a_retryable_rejection` proves
  overflow is rejected before another handler thread is spawned.

The real retained executor tests in
[`crates/rue/src/daemon_qualification_tests.rs`](../../crates/rue/src/daemon_qualification_tests.rs)
cover runtime eviction while owned answers are retained, missing imports and
source-manifest changes, standard-library content and symlink routes,
implicitly acquired standard support, root routes, and archive creation,
corruption, and repair. `daemon_pressure_eviction_reclaims_a_real_runtime_while_its_answer_is_held`
checks the weak runtime is dead after eviction, then consumes the held answer
and compares a rebuilt answer. The retained host tests in
[`crates/rue/src/host_workflow_tests.rs`](../../crates/rue/src/host_workflow_tests.rs)
cover empty and mixed AIR/listing/image/build/error/cancel/repair hosts,
rotating roots, held owned responses, and an alive positive control through the
opaque liveness probe.

These probes establish runtime reclamation through ownership and worker
teardown. A lower RSS reading is not required: allocator high-water memory may
remain in the process after the live query runtime and response leases are
gone.

## Release calibration

The three completed calibration lifetimes used source revision
`15812c1a466ab531f75b538c5381c2490f2d6e87` and release compiler
`709c5d4c9d888474dbdab9982b145225d9a77ac93e8b723f8362ba1029645f9a` with
`release_thin_lto`, internal linking, `-O0`, the native AArch64 macOS target,
and four resolved workers. They covered 60 requests. The primary lifetime had
36 requests across the rotation sequence `startup`, `lattice`, `functions`,
`startup`, `lattice`, `startup`, with build and AIR requests repeated three
times; the `functions` rotation used the 4,096-function fixture. Two further
12-request lifetimes repeated the service workload. Every request matched a
fresh direct answer on output bytes and stderr; each service stopped and its
PID was reaped. The harness did not execute the produced programs.
Its isolated services used an explicit 60-second idle timeout; the production
default remains 30 minutes.

The primary lifetime's maximum retained query charge was 323,961,658 bytes
(about 309 MiB), with 229,492 dependency pins and a 4,240,640-byte response
peak at response-lease acquisition. The two repeat lifetimes measured query
peaks of 323,961,666 and 323,961,662 bytes and response peaks of 4,240,639
bytes. The primary maximum
sampled RSS was 748,432 KiB and its final startup remained at 727,024 KiB;
the repeats reached 712,384 and 713,280 KiB. These are allocator high-water
observations, not evidence that the query graph survived. Weak-liveness tests
provide the separate live-runtime proof.

Every large-function request exceeded the retained-charge budget and evicted
its idle host while preserving the completed answer. Its current host, query,
pin, and source gauges returned to zero. Subsequent startup and Lattice
requests rebuilt and again matched direct output. The 256 MiB opt-in bound
therefore preserves the measured Lattice working set but sacrifices warmth
for this larger fixture; it is not a process RSS cap.

The checked-in summary is
[`daemon-resource-qualification.json`](daemon-resource-qualification.json),
and the harness is
[`scripts/bench-daemon-resources.py`](../../scripts/bench-daemon-resources.py).
This is a resource qualification record, not a speedup claim: it does not
establish cold or warm latency thresholds,
automatic startup, or a cross-platform RSS bound. RUE-2130 owns that regime
and decision.

## Reproduction and limitations

Run the focused targets through the repository wrapper:

```text
RUE="$(scripts/rue-bin --target-platforms //platforms:release)"
python3 scripts/bench-daemon-resources.py "$RUE" "$PWD" /tmp/rue-dqcal-r1 \
  --functions 4096 --repeats 3
python3 scripts/bench-daemon-resources.py "$RUE" "$PWD" /tmp/rue-dqcal-r2 \
  --functions 4096 --repeats 1
python3 scripts/bench-daemon-resources.py "$RUE" "$PWD" /tmp/rue-dqcal-r3 \
  --functions 4096 --repeats 1

scripts/rue unit rue-driver daemon
scripts/rue unit rue daemon
```

The service protocol and tests run on the local Unix-domain transport. Native
AArch64 and Linux CI remain platform authorities. The qualification does not
add parallel request admission, watch transport, automatic daemon selection,
or a system-wide scheduler. It also does not claim that operational RSS and
compiler retained-charge observations are byte-for-byte equivalent metrics;
they intentionally describe different ownership layers.
