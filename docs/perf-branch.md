# Performance Tracking Branch

This document describes the `perf` branch workflow for storing benchmark history.

## Overview

Benchmark results are stored on a dedicated `perf` branch to avoid cluttering the main branch with frequent updates. CI runs benchmarks on each commit to trunk and pushes results to the `perf` branch.

## Branch Structure

The `perf` branch contains:
- `benchmarks/history.json` - Complete benchmark history

## Workflow

### CI Workflow (Automated)

1. On each commit to `main`:
   - CI runs `./bench.sh --no-history --output /tmp/results.json`
   - CI switches to `perf` branch
   - CI runs `./scripts/append-benchmark.py /tmp/results.json benchmarks/history.json`
   - CI commits and pushes to `perf` branch

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
