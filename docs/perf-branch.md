# Performance Tracking Branch

This document describes the `perf` branch workflow for storing benchmark history.

## Overview

Benchmark results are stored on a dedicated `perf` branch to avoid cluttering the main branch with frequent updates. CI runs benchmarks in parallel across platforms and a collector job aggregates results before pushing atomically to the `perf` branch.

The workflow uses artifact-based collection and one atomic Git publication. Individual platform jobs upload artifacts; a single collector enriches them with version 3 publication metadata and pushes all platform histories in one commit.

## Branch Structure

The `perf` branch contains one partitioned history per platform:

- `benchmarks/history-<platform>/index.json` — the atomic snapshot index.
- `benchmarks/history-<platform>/shards/YYYY/MM/<sha256>.json` — immutable per-run shards.

Legacy `history-<platform>.json` files are read once during migration. Their missing metadata remains explicitly unknown and non-comparable.

## Workflow

### CI Workflow (Automated)

1. Benchmarks are triggered by:
   - **Push to trunk**: Triggered on every commit (older queued runs are canceled)
   - **Manual**: workflow_dispatch for on-demand runs

2. When triggered:
   - Three platform jobs run in parallel (x86-64-linux, aarch64-linux, aarch64-macos)
   - Each job:
     - Runs `./bench.sh --no-history --output /tmp/results.json`
     - Uploads results as artifact: `benchmark-results-{commit_sha}-{platform}.json`
   - Collector job runs after all platform jobs complete:
     - Downloads all platform artifacts
     - Checkouts perf branch
     - Appends each platform's results to its history file
     - Commits and pushes once (atomic push, no race conditions)

3. If multiple commits arrive rapidly, cancellation keeps the newest run. The
   collector records the measured commit, every explicitly skipped commit, and
   whether any gap is unknown. Skipped commits are never represented as measured.

**Legacy (Before Phase 1):**

1. On each commit to `trunk`:
   - Three platform jobs ran sequentially (max-parallel: 1)
   - Each job directly pushed to perf branch (caused race conditions and throughput bottleneck)

### Local Workflow (Manual)

To run benchmarks locally and update history:

```bash
# Run benchmarks (auto-appends to the local host's partitioned history)
./bench.sh

# Or save to specific file without updating history
./bench.sh --no-history --output my-results.json

# Manually append to history
python3 scripts/append-benchmark.py my-results.json \
  website/static/benchmarks/history-x86-64-linux \
  --platform x86-64-linux --runner-image local:x86-64-linux --reason manual
```

### Website Build

During website deployment, one `git archive` extracts the complete benchmark
tree from a single perf commit. Chart and status consumers then read the shared
index-and-shard schema before Zola builds the site.

## Why a Separate Branch?

- **Reduced noise**: Benchmark commits don't clutter main branch history
- **Simplified permissions**: CI can push to `perf` without main branch protection issues
- **Easy rollback**: Benchmark history can be reset without affecting code
- **Clean separation**: Code changes and performance data are independent

## History Retention

Logical history is unbounded. Appending creates one immutable content-addressed
run shard and atomically replaces the small index; existing measurements are
never pruned or rewritten.

## Manual Maintenance

> **Note:** The commands below use git rather than jj because the `perf` branch
> is managed by GitHub Actions CI, which uses git. The perf branch exists only
> on the remote and is not part of the normal jj workflow.

History is durable and should not be reset during ordinary maintenance. A
deliberate repair must preserve or explicitly account for every indexed shard
and publish the repaired tree in one perf-branch commit.

To view the current x86-64 Linux index:
```bash
git show perf:benchmarks/history-x86-64-linux/index.json | jq '.run_count'
```
