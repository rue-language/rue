---
id: 0031
title: Robust Performance Testing Infrastructure
status: proposal
tags: [tooling, ci, performance]
feature-flag: null
created: 2026-01-11
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0031: Robust Performance Testing Infrastructure

## Status

Proposal

## Summary

Redesign the performance testing infrastructure to handle high commit velocity (multiple commits per minute) while ensuring complete data collection and eliminating race conditions. The current system uses sequential job execution which creates an ever-growing queue that GitHub Actions cannot process in time.

## Context

### Current Architecture (ADR-0019)

The existing performance testing system (ADR-0019) runs benchmarks on every commit to trunk:

1. GitHub Actions workflow triggers on push to trunk
2. Three platform jobs (x86-64-linux, aarch64-linux, aarch64-macos) run **sequentially** (max-parallel: 1)
3. Each job:
   - Builds the compiler
   - Runs 7 benchmarks with 5 iterations each
   - Appends results to a platform-specific history file on the `perf` branch
   - Commits and pushes to `perf` branch
4. Concurrency control: `cancel-in-progress: false` (jobs queue up instead of canceling)

### The Problem

With **multiple commits per minute** and sequential execution taking **3-6 minutes per commit**, the queue grows faster than it drains:

- Each platform takes ~1-2 minutes to benchmark
- Sequential execution: 3-6 minutes total per commit
- Commit rate: >60 commits per hour
- Processing rate: ~10-20 commits per hour
- **Result:** Queue grows indefinitely, jobs timeout or get dropped, **data is missing**

### Root Causes

1. **Sequential execution bottleneck**: `max-parallel: 1` was added to prevent race conditions on perf branch, but creates throughput bottleneck
2. **Per-commit granularity**: Benchmarking every single commit at high velocity is unsustainable
3. **Git branch as database**: Using the perf branch for atomic updates is slow and prone to conflicts
4. **Lack of batching**: No mechanism to group multiple commits together

### Why Sequential Execution Existed

The original `max-parallel: 1` was added to prevent this race:
```
Job 1 (x86-64):  fetch perf branch → append → push
Job 2 (ARM64):   fetch perf branch → append → push (CONFLICT!)
```

But this "solution" made throughput the limiting factor.

## Decision

### Architecture Overview

Replace the sequential push-to-perf-branch model with a **parallel execution + atomic collection** model:

```
Commit → 3 parallel platform jobs → Artifacts → Collector job → Single atomic push
```

Key principles:
1. **Parallel platform execution**: All 3 platforms run concurrently
2. **Artifact-based data flow**: Platform jobs upload results as artifacts, not git pushes
3. **Atomic collection**: Single collector job fetches artifacts and pushes once
4. **Smart batching**: Debounce rapid commits to reduce load
5. **Graceful degradation**: If queue is too long, skip intermediate commits intelligently

### Part 1: Parallel Platform Execution

**Change:** Remove `max-parallel: 1`, let all 3 platforms run concurrently.

**Implementation:**
- Remove strategy.max-parallel constraint
- Each job stores results as GitHub artifact instead of pushing to perf branch
- Artifact names include commit SHA and platform: `benchmark-results-{sha}-{platform}.json`

**Benefit:** Reduces per-commit time from 3-6 minutes to 1-2 minutes (3x speedup).

### Part 2: Atomic Result Collection

**New job:** `collect-results` that runs after all platform jobs complete.

**Workflow:**
```yaml
jobs:
  benchmark:
    strategy:
      matrix:
        include: [x86-64-linux, aarch64-linux, aarch64-macos]
    steps:
      - run benchmarks
      - upload artifact: benchmark-results-{sha}-{platform}.json

  collect-results:
    needs: benchmark
    steps:
      - download all artifacts
      - checkout perf branch
      - append all results to platform-specific history files
      - commit and push (single atomic push, no race)
```

**Key insight:** Only one job pushes to perf branch, so no race conditions.

### Part 3: Commit Batching / Debouncing

**Problem:** Even with parallelization, benchmarking every commit at 60+/hour is expensive.

**Solution:** Batch multiple commits together using GitHub Actions concurrency groups.

**Strategy A: Time-based batching**
```yaml
concurrency:
  group: benchmarks-${{ github.run_number / 5 }}  # Batch every 5 runs
  cancel-in-progress: true  # Cancel older batches
```

**Strategy B: Scheduled + on-demand**
```yaml
on:
  push:
    branches: [trunk]
  schedule:
    - cron: '*/15 * * * *'  # Every 15 minutes
```
- On push: Debounce and run once per time window
- Scheduled: Ensure coverage even during quiet periods
- Manual: workflow_dispatch for on-demand runs

**Recommendation:** Start with **Strategy B** (scheduled + on-demand) because:
- Predictable resource usage
- Clear sampling frequency
- Easy to reason about
- Can tune frequency based on load

### Part 4: Smart Commit Sampling

For very high velocity, don't benchmark every commit. Use one of:

**Option 1: Latest commit in time window**
- Every 15 minutes, benchmark the most recent commit in that window
- Tag benchmark results with the commit it represents
- Interpolate missing commits in visualization

**Option 2: Merge-commit only**
- Only benchmark merge commits to trunk (not every development commit)
- Requires workflow that merges PRs vs direct push
- Lower frequency, higher signal

**Option 3: Mark commits for benchmarking**
- Allow developers to tag commits that should be benchmarked
- Use commit message convention: `[bench]` or label on PR
- Benchmark tagged commits + scheduled fallback

**Recommendation:** Start with **Option 1** (time window) as it requires no process change.

### Part 5: Data Completeness Tracking

Even with batching, we want visibility into what was benchmarked.

**Add metadata to results:**
```json
{
  "version": 2,
  "commit": "abc123",
  "commit_range": ["abc123", "def456", "789abc"],  // All commits in this batch
  "benchmark_reason": "scheduled" | "manual" | "tagged",
  "timestamp": "...",
  ...
}
```

**Dashboard improvements:**
- Show benchmark coverage (% of commits benchmarked)
- Highlight gaps in data
- Allow filtering by commit range

### Part 6: Graceful Degradation

If the queue still backs up (e.g., during a period of intense development):

**Strategy: Adaptive sampling**
- Monitor queue depth using GitHub API
- If queue > 10 jobs: increase time window (30 min instead of 15)
- If queue > 20 jobs: skip all queued jobs, start fresh with latest commit
- Log all skipped commits for transparency

**Implementation:** Add a pre-job step that checks queue and decides whether to run.

## Implementation Phases

**Epic:** rue-1h38

- [x] **Phase 1: Parallel execution + artifact upload** - rue-1h38.1
  - Modify benchmarks.yml to remove max-parallel constraint
  - Change platform jobs to upload artifacts instead of pushing to perf
  - Verify artifacts are created correctly

- [ ] **Phase 2: Atomic collector job** - rue-1h38.2
  - Add collect-results job that downloads artifacts
  - Implement atomic push to perf branch
  - Test with multiple parallel platform jobs

- [ ] **Phase 3: Time-based batching** - rue-1h38.3
  - Add scheduled trigger (every 15 minutes)
  - Remove push trigger or debounce it
  - Update documentation

- [ ] **Phase 4: Commit range tracking** - rue-1h38.4
  - Update JSON schema to include commit_range
  - Modify append-benchmark.py to handle ranges
  - Update dashboard to show coverage metrics

- [ ] **Phase 5: Graceful degradation** - rue-1h38.5
  - Add queue depth monitoring
  - Implement adaptive sampling logic
  - Add logging for skipped commits

- [ ] **Phase 6: Dashboard improvements** - rue-1h38.6
  - Visualize benchmark coverage
  - Show commit ranges for each benchmark run
  - Highlight data gaps

## Consequences

### Positive

- **Throughput:** 3x faster per-commit time (parallel vs sequential)
- **No race conditions:** Single atomic push eliminates conflicts
- **Scalable:** Handles high commit velocity through batching
- **Complete data:** No dropped jobs, all commits accounted for
- **Graceful degradation:** System adapts to load automatically
- **Transparency:** Clear visibility into what was benchmarked and why

### Negative

- **Complexity:** More moving parts (artifacts, collector job, batching logic)
- **Not per-commit:** Won't have data for every single commit (but we track ranges)
- **Delayed feedback:** 15-minute batching means slower results
- **Storage costs:** GitHub artifact storage (though artifacts expire after 90 days)

### Neutral

- **Different granularity:** From per-commit to per-time-window sampling
- **Requires tuning:** May need to adjust time windows based on usage patterns

## Resolved Questions

1. **How to eliminate race conditions without serialization?**
   - Artifact-based flow with single collector job

2. **How to handle high commit velocity sustainably?**
   - Time-based batching (15-minute windows)

3. **How to maintain data completeness?**
   - Track commit ranges, show coverage metrics

4. **What if the queue still backs up?**
   - Adaptive sampling with graceful degradation

5. **How to make the change incrementally?**
   - 6 phases, each independently valuable

## Open Questions

1. **What's the right time window?** Start with 15 minutes, tune based on data.

2. **Should we keep any per-commit benchmarking?** Could benchmark tagged commits in addition to scheduled runs.

3. **How long to keep artifacts?** GitHub default is 90 days, should we archive to long-term storage?

4. **Should we alert on missing data?** Add monitoring for benchmark gaps exceeding threshold.

## Future Work

- **Benchmark result diffing:** Compare results across commits in UI
- **Performance regression detection:** Automatic alerts for slowdowns
- **Historical trend analysis:** ML-based anomaly detection
- **Cross-platform comparison:** Visualize platform differences
- **Incremental benchmarking:** Only re-benchmark changed components

## References

- [ADR-0019: Compiler Performance Dashboard](0019-performance-dashboard.md) - Original design
- [GitHub Actions: Artifacts](https://docs.github.com/en/actions/using-workflows/storing-workflow-data-as-artifacts)
- [GitHub Actions: Concurrency](https://docs.github.com/en/actions/using-workflows/workflow-syntax-for-github-actions#concurrency)
- [Rust perf infrastructure](https://github.com/rust-lang/rustc-perf) - Similar problems and solutions
