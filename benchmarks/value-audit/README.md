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
