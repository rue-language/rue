# Contribution workflow (jj)

> **Scope: internal/maintainer-oriented.** This describes the maintainers'
> Jujutsu-based repository setup and assumes the configured git remotes and jj
> revset aliases in [Required repo config](#required-repo-config). External
> contributors do **not** need any of this — an ordinary Git fork and a GitHub
> PR against `trunk` is fully supported; see
> [CONTRIBUTING.md](../../CONTRIBUTING.md).

Rue supports two ways to publish a feature branch:

- **Steve and Dorian:** prefer pushing feature branches directly to
  `rue-language/rue` when repository access and the working environment allow
  it. The change still goes through a pull request; never push directly to
  `trunk`.
- **All other contributors:** push feature branches to a personal fork and open
  a pull request into `rue-language/rue`. This is a fully supported workflow.

The examples use these remote names:

- `upstream` = `rue-language/rue` — the canonical repository and source of
  truth.
- `origin` = `<you>/rue` — a personal fork, when one is used.

## Rules

1. **Always base work on `trunk()` (= `trunk@upstream`) and never push
   `trunk`.** A fork's `origin/trunk` does not need to mirror upstream:
   cross-fork PRs diff against `upstream/trunk`, and jj's immutability anchor is
   `trunk@upstream`. `trunk@origin` is untracked (see required config) so it may
   harmlessly remain behind upstream.
2. **Work on a feature change**, then push it as a branch and open a PR. Steve
   and Dorian should use the direct-upstream form when possible:
   ```bash
   jj new 'trunk()'
   # ... make edits ...
   jj git push --remote upstream -c @
   gh pr create --repo rue-language/rue --base trunk --head <branch> ...
   gh pr merge <n> --repo rue-language/rue --auto   # queue it immediately
   ```
   Everyone else uses the same process through a fork:
   ```bash
   jj git push --remote origin -c @
   gh pr create --repo rue-language/rue --base trunk --head <you>:<branch> ...
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
jj config set --repo git.fetch '["origin", "upstream"]'            # when using a fork, fetch both remotes
jj bookmark untrack 'trunk' --remote=origin                        # when using a fork, don't track origin/trunk
```

The revset alias is required in both setups. A checkout without a fork can set
`git.fetch` to `["upstream"]` instead. In a fork-based setup, fetching both
remotes ensures upstream merges are visible, while `untrack` keeps the local
`trunk` bookmark tracking only upstream.
