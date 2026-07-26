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
programs the RUE-1026/RUE-1027 query cutover regressed. Their `//:cli-tests`
cases bound each compile with a per-case contract budget, but the required
corpus only answers pass/fail at that budget. These rows are where cold *cost*
is gated against the pre-cutover reference.

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

### The absolute gate is enforced only without a baseline

An absolute budget compares a wall time on whatever host is running against a
constant calibrated on a different one. There is no multiplier that is
simultaneously tight enough to mean something and loose enough to be safe
everywhere: a measured host ran 2.4x the reference figures and left as little as
1.2x headroom over its own pre-cutover cost
([`docs/notes/pre-cutover-baseline-binary.md`](../../docs/notes/pre-cutover-baseline-binary.md)).

So the runner decides per run, using the same distinct-binary test that
classifies the comparison:

| historical baseline binary | absolute cold budget |
|---|---|
| absent, or identical to current | **enforced** — a breach fails the scenario |
| distinct | **advisory** — reported, does not decide the verdict |

With a distinct baseline the role-vs-role pair comparison is available and is
host-independent by construction, because both roles run on the same machine in
the same run. The absolute budget then adds nothing the pairs do not already
carry and can only contribute a false failure on a slow host. The run records
which mode applied in `comparison_provenance.absolute_cold_budget`.

An advisory is an observation, never a verdict: it cannot fail a scenario, and
it cannot manufacture a pass for a scenario that has no passing evidence of its
own. A genuine pair failure still fails.

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
