# Benchmark metric semantics

`scripts/benchmark_metrics.py` is the canonical, renderer-independent policy
for interpreting durable benchmark history. Charts and status data consume its
derived JSON; they do not define regression thresholds themselves.

Each latency point is the median of at least three raw `samples_ms`. Observed
variation is `1.4826 * median(abs(sample - median)) / median`. A comparison's
variation band is the root-sum-square of current and reference relative
variation. Lower latency outside that band is `improved`, higher latency is
`regressed`, and movement inside it is `stable`. Missing evidence is
`insufficient_data`; there is no fixed absolute or percentage threshold.
The multi-workload headline delta is the geometric mean of workload ratios;
its variation band is the root-mean-square of workload variation bands.

The rolling baseline is the median of per-run medians from the preceding three
to seven points. Its variation includes both median within-run variation and
scaled MAD between run medians. Comparisons never cross a durable-history
regime or gap and require identical workload composition.

The latency performance index is the geometric mean of each workload's
`baseline median / current median`, multiplied by 100. The baseline is exactly
100 and higher is better. Regime, scenario, or workload changes explicitly
rebase instead of moving the headline. Latency, source throughput, peak memory,
and binary size are emitted as separate metric families and are never combined.

Historical `bench.sh` data is identified as `compiler/cold_compilation`; new
runs record that identity explicitly. Comparison APIs reject a different
measurement or scenario family, reserving distinct reused-session/query
scenarios for RUE-901 without conflating them with cold compilation.

Representative compiler scenarios are a separate publication family. Their
latency is descriptive; correctness and cache claims come from exact emitted
output/fresh-session parity and direct `CompilerSession` structural counters.
They measure compiler build/query work, never generated-program runtime, and do
not feed the static phase-probe performance index or scaling budgets.
