---
id: 0019
title: Compiler Performance Dashboard
status: proposal
tags: [tooling, website, performance]
feature-flag: null
created: 2025-12-27
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0019: Compiler Performance Dashboard

## Status

Proposal

## Summary

Create a system for tracking Rue compiler performance over time and displaying it on the website. This includes a benchmark corpus of representative Rue programs, infrastructure to collect and store timing data, and interactive visualizations for the website.

## Context

As Rue develops, we want to track compiler performance to:
1. **Detect regressions** - Notice when changes slow down compilation
2. **Track improvements** - See the impact of optimization work
3. **Provide transparency** - Show the community our performance characteristics

The existing `--time-passes` flag provides per-pass timing, but there's no:
- Standardized benchmark corpus
- Historical data storage
- Visualization infrastructure

## Decision

### Part 1: Benchmark Corpus

**Location:** `benchmarks/` directory at repository root

**Design choices:**

**Decision: Hand-crafted benchmark programs**

Spec tests are too small to meaningfully benchmark. We'll create purpose-built programs that exercise different compiler phases at scale.

**Corpus structure:**
```
benchmarks/
├── README.md           # Describes the benchmark suite
├── stress/             # Hand-crafted stress tests
│   ├── many_functions.rue    # 100+ functions
│   ├── deep_nesting.rue      # Deeply nested blocks/expressions
│   ├── large_structs.rue     # Many struct types with fields
│   ├── arithmetic_heavy.rue  # Lots of arithmetic expressions
│   └── control_flow.rue      # Complex if/while/match patterns
└── manifest.toml       # Benchmark metadata
```

**Manifest format:**
```toml
[[benchmark]]
name = "spec_tests"
type = "spec_aggregate"  # Run all spec tests, aggregate timing
description = "Aggregate timing across all spec tests"

[[benchmark]]
name = "many_functions"
path = "stress/many_functions.rue"
description = "100 trivial functions to stress function handling"

[[benchmark]]
name = "deep_nesting"
path = "stress/deep_nesting.rue"
description = "20 levels of nested blocks"
```

### Part 2: Data Collection & Storage

**CLI addition:** `--benchmark-json <file>` flag

When provided, outputs structured JSON timing data instead of human-readable report.

**JSON output format:**
```json
{
  "version": 1,
  "timestamp": "2025-12-27T10:30:00Z",
  "commit": "abc123def",
  "host": {
    "os": "darwin",
    "arch": "aarch64",
    "cpu": "Apple M1 Pro"
  },
  "benchmarks": [
    {
      "name": "many_functions",
      "iterations": 5,
      "passes": {
        "lexer": { "mean_ms": 0.5, "std_ms": 0.02 },
        "parser": { "mean_ms": 2.1, "std_ms": 0.1 },
        "astgen": { "mean_ms": 1.2, "std_ms": 0.05 },
        "sema": { "mean_ms": 3.4, "std_ms": 0.15 },
        "cfg": { "mean_ms": 0.8, "std_ms": 0.03 },
        "codegen": { "mean_ms": 5.2, "std_ms": 0.2 },
        "linker": { "mean_ms": 1.0, "std_ms": 0.04 }
      },
      "total_ms": { "mean": 14.2, "std": 0.3 }
    }
  ]
}
```

**Data storage:**

**Decision: Dedicated `perf` branch**

Since benchmarks run on every commit to trunk, storing results directly in the main branch would create noise. Instead:

- A dedicated `perf` branch holds benchmark history
- CI runs benchmarks on each trunk commit, pushes results to `perf` branch
- `website/static/benchmarks/history.json` is built from the `perf` branch during website deploy
- Keep the last 100 runs to limit file size

**Benchmark runner script:** `./bench.sh`
```bash
#!/bin/bash
# Build release compiler, run benchmarks, append to history
./buck2 build //crates/rue:rue --release
./buck2 run //crates/rue:rue -- --benchmark-json /tmp/bench.json benchmarks/
# Append to history (via a small Rust tool or script)
./scripts/append-benchmark.py /tmp/bench.json website/static/benchmarks/history.json
```

### Part 3: Website Visualization

**Page:** `/performance/` on the Rue website

**Decision: Static charts generated at build time**

Keep scope minimal with static SVG charts generated during website build. We can add Chart.js for interactivity later if needed.

**Charts to generate:**
1. **Time-series chart** - Total compilation time over commits
2. **Pass breakdown chart** - Stacked bar showing time per pass

**Implementation:**
- A Rust tool or Python script reads `history.json` and generates SVG charts
- Charts are placed in `website/static/benchmarks/` during build
- Zola includes them in the performance page

**Template structure:**
```
website/
├── content/
│   └── performance.md       # Performance page content
├── templates/
│   └── performance.html     # Template including chart images
├── static/
│   └── benchmarks/
│       ├── history.json     # Historical data (from perf branch)
│       ├── timeline.svg     # Generated time-series chart
│       └── breakdown.svg    # Generated pass breakdown
```

**Future enhancement:** Add Chart.js for hover tooltips and filtering when scope allows

## Implementation Phases

**Epic:** rue-a5ah

- [ ] **Phase 1: Benchmark corpus** - rue-a5ah.1
  - Create `benchmarks/` directory structure
  - Write initial stress test programs (many_functions, deep_nesting, etc.)
  - Create `manifest.toml` format

- [ ] **Phase 2: Data collection** - rue-a5ah.2
  - Implement `--benchmark-json` flag in the CLI
  - Extend `TimingData` to output JSON format
  - Support multiple iterations with mean/std calculation

- [ ] **Phase 3: Runner & storage** - rue-a5ah.3
  - Create `./bench.sh` script
  - Create `scripts/append-benchmark.py` to manage history
  - Set up `perf` branch structure
  - Document benchmark workflow

- [ ] **Phase 4: CI integration** - rue-a5ah.4
  - GitHub Actions workflow to run benchmarks on trunk commits
  - Push results to `perf` branch
  - Configure consistent benchmark environment

- [ ] **Phase 5: Website visualization** - rue-a5ah.5
  - Create SVG chart generator (Rust or Python)
  - Create `/performance/` page template
  - Integrate chart generation into website build

## Consequences

### Positive
- Clear visibility into compiler performance over time
- Ability to detect regressions before they accumulate
- Public transparency about performance characteristics
- Foundation for future optimization work
- CI runs on every commit ensures continuous tracking

### Negative
- CI runner variability may add noise (mitigated by multiple iterations)
- Adds maintenance burden (corpus, scripts, visualization)
- History file will grow over time (mitigated by limiting to 100 entries)

### Neutral
- Benchmarks measure compilation time, not runtime performance
- Initial corpus may not represent "real-world" usage until we have users
- Dedicated `perf` branch keeps main branch clean but adds complexity

## Resolved Questions

1. **Multiple iterations?** Yes, 5 iterations with mean/std reported.

2. **What stress tests?** Initial set: many_functions, deep_nesting, large_structs, arithmetic_heavy, control_flow.

3. **History retention?** 100 most recent runs.

4. **Visualization approach?** Static SVG charts for now, Chart.js as future enhancement.

5. **When to run benchmarks?** On every commit to trunk via CI.

6. **Data storage?** Dedicated `perf` branch to avoid noise in main.

## Future Work

- Interactive charts with Chart.js (hover tooltips, filtering, zoom)
- Runtime performance benchmarks (execution speed of compiled programs)
- Memory usage tracking
- Code size tracking (binary size over time)
- Comparison with other compilers (Rust, Zig, etc.)

## References

- [ADR-0018: Tracing Infrastructure](0018-tracing-infrastructure.md) - The timing layer this builds on
- [rustc-perf](https://perf.rust-lang.org/) - Inspiration from Rust's approach
- [Zig perf](https://ziglang.org/perf/) - Inspiration from Zig's approach
