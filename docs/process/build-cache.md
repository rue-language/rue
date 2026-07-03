# Remote build cache (BuildBuddy)

Buck2 rebuilds everything from scratch in each `buck-out`, and OSS buck2 has **no
persistent action cache across daemon restarts** (noted in `ci.yml`). That means
every CI run and every isolated worktree rebuilds unchanged crates. A remote
action cache fixes both. We use **BuildBuddy** (free tier), **cache-only**: actions
still execute locally, only the action cache + CAS are remote — so it can only
affect *speed*, never correctness (a bad entry is at worst a cache miss's worth of
wrong; local execution is the source of truth for anything not already cached).

Tracking: RUE-316.

## Local dev

```bash
cp .buckconfig.local.example .buckconfig.local
# paste your key (https://app.buildbuddy.io -> Settings -> API keys) into the
# x-buildbuddy-api-key line
```

`.buckconfig.local` is gitignored (holds the key) and buck2 layers it over
`.buckconfig` automatically. It sets `[buck2_re_client]` (pointed at
`remote.buildbuddy.io`) and routes execution through `root//platforms:remote_cache`
(local execution + remote cache). Without the file, nothing changes — the shared
config stays cache-agnostic.

## CI

CI reads the key from the `BUILDBUDDY_API_KEY` repo secret (never from a file).
The `cache-probe` workflow (`.github/workflows/cache-probe.yml`) writes a
transient `.buckconfig.local` from the secret, does a cold build then a
clean-and-rebuild, and reports buck2's `Commands: (cached / remote / local)`
line — the second build should show remote hits. Run it with
`gh workflow run cache-probe.yml` on a repo that has the secret. Once it's proven
green with real hit-rates, the cache config can be promoted into the main build
and test jobs.

## Why cache-only, not remote execution

Remote *execution* (offloading compiles to the cloud) needs hermetic actions +
platform matching, and our toolchains-as-a-separate-cell setup already made
configuration subtle (RUE-277). Cache-only gets most of the win (skip unchanged
rebuilds) with none of that risk. Revisit RE later if the cache proves out.
