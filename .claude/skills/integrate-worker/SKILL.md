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

5. **A tightening change compiles the example corpus BEFORE the suite.**
   `./buck2 build //:cli-staged-programs` builds all ten multi-file example
   programs. Any change that makes the compiler reject, trap, or error where it
   previously accepted needs this, and canaries are not enough: a RUE-1786
   over-rejection passed `examples/harbor` *and* its own mutation check, and was
   caught only by `examples/gazette` — an outer `borrow self` receiver holding a
   loan while an inner call takes the same root `inout`, which is sound because
   a receiver is address-passed. Minutes against ~20 for the full suite, and it
   is exactly where over-rejection surfaces. A mutation check proves the fix
   DOES something; it says nothing about what the fix breaks.

6. **Commit, then full suite + format.** `./test.sh` must exit 0; `bash
   scripts/rue fmt`. Commit *before* starting the suite and do not touch the
   checkout while it runs — switching branches mid-run once produced a
   fabricated `E0583: file not found for module comptime` citing a line that
   existed only on trunk, and cost an hour of chasing a bug that was not there.
   Read the banner: `=== TEST SUITE: PASSED ===` is now asserted rather than
   inferred, and a killed run says `INTERRUPTED (SIG..)` instead (RUE-1782).
   Do NOT judge by the tally alone — Buck can return cached results, so a run
   stopped partway can print a complete-looking `Tests finished: ... Fail 0`
   and still not have finished. The banner is what distinguishes them.

7. **Ship + queue.** `jj describe` (subject + why + what was verified),
   `jj git push -c @`, `gh pr create --repo rue-language/rue --base trunk
   --head steveklabnik:<bookmark>` with one `Fixes RUE-NN` **per line** (a
   comma list only closes the first), `gh pr merge <n> --auto`. Use
   `Part of RUE-NN` when items remain — and remember "Part of" strands the
   issue In Progress (sweep afterwards per CLAUDE.md).

8. **Queue bounces** (DIRTY after a sibling merges): `jj rebase -s <change>
   -d 'trunk()'`, resolve keeping BOTH PRs' semantics, re-run step 3's repros
   plus the sibling's, full suite, push the bookmark, re-arm `--auto`.
   Never force-push a branch that is actively queued without re-arming.

   **On MERGE_CONFLICT, check trunk for the issue ID first:**
   `git log origin/trunk --format='%h %s' | grep -E '^[0-9a-f]+ [^:]*RUE-NN'`.
   Anchor on the SUBJECT — plain `--grep=RUE-NN` searches the whole message and
   happily matches commits that merely *cite* the issue in their body, which is
   a false "it already landed" on exactly the question you are asking.

   A dequeue is not always a real conflict — RUE-1766 landed on trunk
   independently while a PR carrying it sat in the queue, and every conflicting
   file was that now-redundant half.
   There the resolution is to DROP the redundant half and keep only what is
   still unique to your branch, not to reconcile two spellings of one fix; then
   confirm the remaining half is still unlanded before assuming the PR has
   anything left to say. The same collision shows up in other disguises —
   `mergeable_state: dirty` on a PR you have not queued yet, or 3-way conflicts
   at apply time — so make the trunk check reflexive whenever a diff fights you
   in a file it should own.

## Lane discipline (when dispatching parallel workers)

**Check open PRs for each target issue ID before dispatching**, not just Linear
status. Linear tells you what is *Done*; it does not tell you what is *in
flight*. In one night four issues were fixed in parallel by other agents while
workers were already on them — RUE-1711 (across four separate PRs), RUE-1745,
RUE-1646 and RUE-1766 — costing two branch rebuilds, one merge-queue dequeue,
one duplicate issue filed, and most of a worker's output twice over. A
`search_pull_requests` for the issue ID, or
a scan of `list_pull_requests --state open`, costs one call per cluster and
catches all of it. Re-check at integration time too: a fix can land while your
worker is running.

Parallelize across **disjoint crates**, never across the same hot file. The
conflict magnets — `rue-air/src/sema/analysis.rs`, the `rue-error` E-code/preview
registry, and both `codegen/*/cfg_lower.rs` — should be **serialized** (land one,
then dispatch the next in that lane). Most integration pain this project has hit
(stale-base 3-way conflicts, the stabilization `--preview` miss) was two workers
in the same hot file. A worker's cluster should name its crate-set; two clusters
in the same hot lane go in consecutive cycles, not the same one.

## Keeping the tree tidy

- Prefer `Fixes RUE-NN` over `Part of` whenever the PR actually closes the issue —
  `Part of` strands it In Progress and you must sweep it later.
- After a batch of merges, run **`scripts/jj-tidy`** — it deletes orphaned
  `worktree-wf_*`/`cycle*` branches, merged `push-*` bookmarks, and abandons
  dangling changes (safe: git protects checked-out branches; only unbookmarked
  non-`@` heads are abandoned). Without it, `jj log` fills with dozens of dead
  heads within a session.
- For disk pressure, run **`scripts/rue storage clean`** (guard-gated, reclaims
  stale Buck outputs host-wide; it never deletes worktrees or source files) —
  never blanket `rm -rf .claude/worktrees`, which races running workers.

## Worker-prompt invariants this protocol assumes

Workers were told: reproduce first, refutations are as valuable as fixes,
file ownership is by region, output is `git diff <base> HEAD` to an OUTFILE
plus a `.meta` summary. If integrating a diff produced under weaker rules,
treat every claim as unverified.
