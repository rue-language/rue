# Recent benchmark regression workspace

`scripts/benchmark_recent.py` builds the renderer-neutral model used by the
detailed performance page. It takes durable RUE-894 runs, RUE-895 semantic
results, and the single RUE-896 annotation stream; browser code only selects
and renders model entries.

The model retains at most 100 measured points and offers 20, 50, and 100 point
windows. Every point carries authoritative commit metadata and links, measured
and skipped coverage separately, relative and wall-clock freshness (stale
after 48 hours), comparability, applicable
annotations, raw samples, and robust uncertainty. Skipped commits are never
represented as measurements and comparison boundaries remain explicit.

Selectable comparisons are any ordered pair inside one uninterrupted canonical
segment; gaps, regimes, workload sets, and scenarios are never crossed. Their canonical
explanation includes per-workload robust deltas, compiler-pass contributions,
and separate memory and binary-size changes. The default is the latest valid
pair. Per-workload small multiples replace the overplotted seven-line chart;
tables expose the same raw values and uncertainty to keyboard and screen-reader
users. The page defaults to the first platform containing data, while the
cross-platform view remains available explicitly.
