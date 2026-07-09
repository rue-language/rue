# Fork workflow (jj)

How to work on Rue from a **fork** using Jujutsu, with two git remotes:

- `upstream` = `rue-language/rue` — the canonical repo, the source of truth.
  You **cannot** push here; you open PRs into it.
- `origin` = `<you>/rue` — your fork. You push feature branches here, then PR
  them upstream.

This documents the maintainers' working setup (originally `steveklabnik/rue`);
adjust names for your own fork.

## Rules

1. **Always base work on `trunk()` (= `trunk@upstream`); do NOT push or sync
   `origin/trunk`.** You never need to mirror `origin/trunk` to upstream —
   cross-fork PRs diff against `upstream/trunk`, and jj's immutability anchor is
   `trunk@upstream`. So `origin/trunk` may sit stale (behind upstream); that's
   harmless. `trunk@origin` is untracked (see required config) precisely so you
   aren't tempted to push it. Never commit on `trunk` / PR `trunk` — that causes
   hash-rewrite divergence when upstream rebase/squash-merges.
2. **Work on a feature change**, then push it as a branch and PR it:
   ```bash
   jj new 'trunk()'                # start the change on upstream's canonical trunk (a revset, not a bookmark)
   # ... make edits ...
   jj git push -c @                # pushes as <you>/push-<changeid> (see git_push_bookmark template)
   gh pr create --repo rue-language/rue --base trunk --head <you>:<branch> ...
   gh pr merge <n> --repo rue-language/rue --auto   # queue it immediately
   ```
3. **`trunk()` is a revset alias = `trunk@upstream`** — always means upstream's
   latest, regardless of local bookmark state. Always use `trunk()`, never the
   bare `trunk` bookmark, in `jj new`/rebase/log commands.
4. **After a PR merges**, the only step is: `jj git fetch` (your local `trunk`
   fast-forwards to upstream), then `jj new 'trunk()'` to start the next change.
   Do **not** push `trunk` to origin — there's nothing to sync. If upstream
   rebase-merged (rewriting hashes), the old fork-side copies show as
   "divergent" — cosmetic; `jj abandon` the orphaned old-hash chain to tidy up.

## Required repo config

Machine-local; set on a fresh clone — jj does not read committed config:

```bash
jj config set --repo 'revset-aliases."trunk()"' 'trunk@upstream'   # base/immutability = canonical repo
jj config set --repo git.fetch '["origin", "upstream"]'            # always see both remotes
jj bookmark untrack 'trunk' --remote=origin                        # don't track/sync origin/trunk; base on upstream only
```

Without the first two, `jj git fetch` only pulls `origin` (you won't see
upstream merges), and `trunk()`/immutability anchor to your fork instead of
upstream. The `untrack` keeps the local `trunk` bookmark tracking *only*
`upstream`, so it fast-forwards to upstream on fetch and you never feel
obligated to push it back to origin.
