# Benchmark evolution invariants

The long-term dashboard is a renderer-neutral view over the same durable run
objects used by the recent regression workspace. It does not own measurement
or comparison policy.

- `benchmark_metrics.derive_history_metrics()` is the sole source of normalized
  performance indexes, workload indexes, rebases, and absolute latency details.
- `benchmark_annotations.normalized_annotation_stream()` is the sole milestone
  and measurement-identity event stream.
- The 30-day, 90-day, one-year, and all-history windows use UTC calendar time
  ending at the newest durable measurement, never a number of commits. One year
  means the prior calendar anniversary; February 29 maps to February 28.
- Every durable raw measurement remains in `raw_points`. Deterministic thinning
  applies only to `rendered_raw_points`; boundary points are never removed.
- Daily and weekly trends use the canonical `benchmark_metrics.robust_summary()`
  median and scaled MAD. Aggregation keys include
  the canonical comparable segment, so gaps, corpus changes, environment
  changes, and rebases cannot be interpolated.
- Cross-platform presentation overlays only normalized index trends and labels
  every machine as a separate series. Absolute milliseconds remain available in
  the accessible detail table and are never the default cross-machine axis.
- Until representative workload-family metadata exists, each current workload
  is exposed as an explicit `identity:<name>` fallback family. The model accepts
  future declared `workload_family` metadata without inventing application
  categories today.
- Latency evolution includes only annotations scoped to `latency` or all metrics.
  Memory, binary-size, and throughput-only events remain in their own products.

Browser code performs presentation scaling and filtering only. It does not
calculate deltas, indexes, uncertainty classifications, calendar membership,
aggregation buckets, or comparison boundaries.
