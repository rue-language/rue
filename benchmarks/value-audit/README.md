# Incrementality value audit

`manifest.toml` pins the user-value workload matrix while the canonical
benchmark semantics stay in [`../manifest.toml`](../manifest.toml). Run the
orchestrator with three release-mode compiler binaries:

```sh
scripts/rue-value-audit.py \
  --historical-baseline /path/to/historical/rue \
  --current /path/to/current/rue \
  --candidate /path/to/candidate/rue \
  --source-dir historical_baseline=/path/to/historical/source \
  --source-dir current_production=/path/to/current/source \
  --source-dir candidate=/path/to/candidate/source \
  --session-bench historical_baseline=/path/to/historical/session-bench \
  --session-bench current_production=/path/to/current/session-bench \
  --session-bench candidate=/path/to/candidate/session-bench \
  --scaling-bench current_production=/path/to/current/scaling-bench \
  --output results/value-audit.json
```

The runner performs the manifest-pinned unrecorded warmup and seven alternating
paired samples. It records medians and median absolute deviations (MADs),
explicit per-role source/build provenance linked to executable hashes, host
provenance, raw sample order, and explicit unsupported or indeterminate
scenarios. The source directory is mandatory; a role is never silently mapped
to the candidate checkout.

The current production binary is the recommended baseline for candidate value.
An older pre-query revision may use the cold executable-only fallback when it
cannot consume `--benchmark-json`; cross-protocol timing comparisons are then
indeterminate, and its missing warm evidence is unsupported—not silently
treated as zero work. A run using one binary for all three roles is classified
as `same_binary_protocol_smoke`; it validates protocol and fail-closed gates,
not historical/current/candidate value.

## Regressed-example cold workloads

`rill`, `mosaic`, `harbor`, `lattice`, and `meridian` are the five large example
programs the RUE-1026/RUE-1027 query cutover regressed. All five are `--skip`ped
from `//:cli-tests` and the large-example compile guard is stubbed, so this audit
is currently the only place their cold cost is gated.

Each carries an absolute `cold_wall_seconds` gate derived from the checked-in
`[historical_reference]` table, which transcribes the pre-cutover and
post-cutover figures from
[`docs/notes/rue-1083-closure-evidence.md`](../../docs/notes/rue-1083-closure-evidence.md).
That reference is prose evidence from a local release-build comparison on the
maintainer's host — it is not a run of this protocol, it is not attributable to
a role binary, and the runner refuses to let it be declared role evidence.

Two properties make these rows useful before a historical baseline binary
exists:

- The gate is **per-role and absolute**, so it produces a real verdict even in a
  `same_binary_protocol_smoke` where every pair is `unsupported`. Role-vs-role
  cold comparison still needs the pre-cutover binary; the gate does not.
- `cold_timeout_seconds` is deliberately far above the gate, so an over-budget
  compile is recorded as a gate failure instead of raising out of the run.

Cost: these are cold-driver workloads under the same one-warmup, seven-paired-
sample protocol as every other row, and `meridian` currently compiles in minutes
rather than seconds. Expect the full matrix to run for hours on one host, in the
same class as the existing Caldera row. Use `--workloads` to select a subset
while iterating.

The existing session benchmark emits versioned full-parity evidence and
production computed/reused/invalidated counters. Required warm locality gates
use manifest-authoritative exact or upper bounds; full-program reparsing,
semantic reruns, or broad body/CFG recomputation fail closed. The reverse/fanout
fixture has a bounded manifest allowance. Medium ordinary Rue and Caldera are
cold-driver workloads. A repeated-edit RSS protocol is explicitly optional and
reported unsupported until an existing benchmark binary exposes a
persistent-session repeated-edit mode; the runner does not invent a
fresh-process proxy for retained memory. The scaling timing binary does not
expose per-body production counters, so its timing is operational evidence and
cannot by itself prove flat per-body work.
