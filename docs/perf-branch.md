# Performance Tracking Branch

This document describes the `perf` branch workflow for storing benchmark history.

## Overview

Benchmark results are stored on a dedicated `perf` branch to avoid cluttering the main branch with frequent updates. CI runs benchmarks in parallel across platforms and a collector job aggregates results before pushing atomically to the `perf` branch.

**Architecture (ADR-0031 Phase 1 + 2):** The workflow uses artifact-based collection with atomic pushing. Individual platform jobs upload artifacts, and a single collector job downloads all artifacts and pushes to perf branch once. This eliminates race conditions while enabling parallel execution.

## Branch Structure

The `perf` branch contains:
- `benchmarks/history.json` - Complete benchmark history

## Workflow

### CI Workflow (Automated)

**Current (Phase 1 + 2 - Parallel execution with atomic collection):**

1. On each commit to `trunk`:
   - Three platform jobs run in parallel (x86-64-linux, aarch64-linux, aarch64-macos)
   - Each job:
     - Runs `./bench.sh --no-history --output /tmp/results.json`
     - Uploads results as artifact: `benchmark-results-{commit_sha}-{platform}.json`
   - Collector job runs after all platform jobs complete:
     - Downloads all platform artifacts
     - Checkouts perf branch
     - Appends each platform's results to its history file
     - Commits and pushes once (atomic push, no race conditions)

**Legacy (Before Phase 1):**

1. On each commit to `trunk`:
   - Three platform jobs ran sequentially (max-parallel: 1)
   - Each job directly pushed to perf branch (caused race conditions and throughput bottleneck)

### Local Workflow (Manual)

To run benchmarks locally and update history:

```bash
# Run benchmarks (auto-appends to website/static/benchmarks/history.json)
./bench.sh

# Or save to specific file without updating history
./bench.sh --no-history --output my-results.json

# Manually append to history
./scripts/append-benchmark.py my-results.json website/static/benchmarks/history.json
```

### Website Build

During website deployment:
1. Fetch `history.json` from `perf` branch
2. Copy to `website/static/benchmarks/history.json`
3. Generate charts from history (Phase 5)
4. Build website with Zola

## Why a Separate Branch?

- **Reduced noise**: Benchmark commits don't clutter main branch history
- **Simplified permissions**: CI can push to `perf` without main branch protection issues
- **Easy rollback**: Benchmark history can be reset without affecting code
- **Clean separation**: Code changes and performance data are independent

## History Retention

- Maximum 100 benchmark runs are retained
- Older results are automatically pruned by `append-benchmark.py`
- This limits `history.json` to approximately 100KB

## Manual Maintenance

> **Note:** The commands below use git rather than jj because the `perf` branch
> is managed by GitHub Actions CI, which uses git. The perf branch exists only
> on the remote and is not part of the normal jj workflow.

To reset benchmark history:
```bash
# Delete and recreate perf branch
git branch -D perf
git checkout --orphan perf
echo '{"version": 1, "runs": []}' > benchmarks/history.json
git add benchmarks/history.json
git commit -m "Reset benchmark history"
git push -f origin perf
```

To view current history:
```bash
git show perf:benchmarks/history.json | jq '.runs | length'
```
