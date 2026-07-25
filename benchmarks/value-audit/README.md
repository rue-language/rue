# Incrementality value audit

`manifest.toml` pins the user-value workload matrix while the canonical
benchmark semantics stay in [`../manifest.toml`](../manifest.toml). Run the
orchestrator with three release-mode compiler binaries:

```sh
scripts/rue-value-audit.py \
  --historical-baseline /path/to/historical/rue \
  --current /path/to/current/rue \
  --candidate /path/to/candidate/rue \
  --session-bench historical_baseline=/path/to/historical/session-bench \
  --session-bench current_production=/path/to/current/session-bench \
  --session-bench candidate=/path/to/candidate/session-bench \
  --scaling-bench current_production=/path/to/current/scaling-bench \
  --output results/value-audit.json
```

The runner performs one unrecorded warmup and seven alternating paired
samples. It records medians and median absolute deviations (MADs), commit and
source hashes, executable hashes, host provenance, raw sample order, and
explicit unsupported scenarios.

The current production binary is the recommended baseline for candidate value.
An older pre-query revision may use the cold executable-only fallback when it
cannot consume `--benchmark-json`; its missing warm evidence is unsupported,
not silently treated as zero work. The report's historical result is context,
not post-flip evidence.

The existing session benchmark currently exposes bounded no-op, unrelated,
leaf-body, and reverse/fanout locality scenarios for the synthetic corpus, and
no-op, leaf-body, and import/fanout scenarios for the representative corpus.
Medium ordinary Rue and Caldera are cold-driver workloads. A repeated-edit RSS
protocol is intentionally reported unsupported until an existing benchmark
binary exposes a persistent-session repeated-edit mode; the runner does not
invent a fresh-process proxy for retained memory.
