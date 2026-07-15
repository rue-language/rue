# Benchmark annotations

Curated performance events live in `benchmarks/annotations.toml`. The file is
version controlled and validated by `scripts/benchmark_annotations.py`; chart,
comparison, and homepage-status metadata all consume its one normalized event
stream.

Each `[[annotation]]` identifies either `commit`, or an inclusive
`start_commit`/`end_commit` range. It includes one of these categories:
`capability`, `optimization`, `benchmark_corpus`,
`measurement_infrastructure`, `known_regression`, or `repair`. A title, concise
explanation, Rue issue or pull-request link, and exact metric/workload/platform
scope are required. `comparability_note` is optional. Use `"*"` alone for an
unrestricted scope dimension.

Commits must resolve in the Rue repository, ranges must follow ancestry, and
duplicate or conflicting events fail the build. Multiple distinct events at a
commit are valid and normalized deterministically.

The normalizer also compares every pair of durable-history publication
regimes. Any corpus, timing schema, build mode, iteration policy, runner,
platform, scenario, or future regime-dimension change automatically creates a
`measurement_infrastructure` event containing its before/after values. This
makes comparability-boundary annotations structural rather than an authoring
convention. Missing annotation files and old histories without publication
regimes remain valid; an entirely legacy history has no derived events, while
either transition between unknown legacy identity and an explicit regime is
annotated as a comparability boundary with the absent identity shown as
`unknown`.
