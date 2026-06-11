---
name: integrate-worker
description: Integrate a worker agent's diff onto fresh trunk — apply, verify repros by hand, cross-check interacting mechanisms, full suite, ship + queue. Use after any worktree worker (fix cycle, hunt follow-up) returns a diff file.
---

# Integrating a worker diff

Battle-tested protocol from the 2026-06 autonomous runs (~30 PRs integrated).
Each step exists because skipping it once caused a real failure.

## Steps

1. **Fresh trunk, always.** `jj git fetch && jj new 'trunk()'` then
   `git reset --quiet` — the colocated git index goes stale after jj
   working-copy switches and makes `git apply --3way` fail with
   "does not match index".

2. **Apply with 3-way fallback.** `git apply --3way /tmp/<worker>.diff`.
   Conflicts mean the worker's base predates trunk changes — resolve by
   *intent*, not by side: a worker editing code that trunk deleted usually
   means the worker's edit is subsumed (keep the deletion); append-append
   conflicts in test TOML files keep both case sets.

3. **Re-verify the repros yourself.** Build (`bash scripts/rue build`) and run
   every repro the worker claims to fix, on this checkout, by execution. A
   worker's "full suite green" was true *in its worktree against its base* —
   not here. Workers have shipped fixes verified against stale drafts of
   trunk; only integration-time re-verification catches that.

4. **Write the cross-mechanism test.** If this worker and another (landed or
   in-flight) built *interacting* mechanisms — e.g. one added per-field drop
   flags while another added drop-on-overwrite — write a test that exercises
   the combination NOW. The workers' own suites cannot see the seam; twice in
   one night, individually-green workers were jointly wrong (double-drops).

5. **Full suite + format.** `./test.sh` must exit 0; `bash scripts/rue fmt`.

6. **Ship + queue.** `jj describe` (subject + why + what was verified),
   `jj git push -c @`, `gh pr create --repo rue-language/rue --base trunk
   --head steveklabnik:<bookmark>` with one `Fixes RUE-NN` **per line** (a
   comma list only closes the first), `gh pr merge <n> --auto`. Use
   `Part of RUE-NN` when items remain — and remember "Part of" strands the
   issue In Progress (sweep afterwards per CLAUDE.md).

7. **Queue bounces** (DIRTY after a sibling merges): `jj rebase -s <change>
   -d 'trunk()'`, resolve keeping BOTH PRs' semantics, re-run step 3's repros
   plus the sibling's, full suite, push the bookmark, re-arm `--auto`.
   Never force-push a branch that is actively queued without re-arming.

## Worker-prompt invariants this protocol assumes

Workers were told: reproduce first, refutations are as valuable as fixes,
file ownership is by region, output is `git diff <base> HEAD` to an OUTFILE
plus a `.meta` summary. If integrating a diff produced under weaker rules,
treat every claim as unverified.
