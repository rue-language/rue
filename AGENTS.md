# Rue repository instructions

This is the canonical guidance for coding agents working in Rue. Tool-specific
instruction files (`CLAUDE.md` and the like) point here rather than copying
these rules, and nothing here assumes a particular agent product.

Rue is an early-stage systems programming language implemented in Rust. It is
built with Buck2, tracked in Linear (`RUE-NN`), and contributed to through
GitHub pull requests into `trunk` behind a merge queue. Maintainers use
Jujutsu in their own checkouts; every other checkout uses Git.

## Quickstart

`scripts/rue` wraps the common Buck2 targets and runs from anywhere in the
repository. Buck2 and the Rust toolchain bootstrap themselves on first use.

```bash
scripts/rue build                    # build the compiler and print its path
scripts/rue exec prog.rue            # compile and run a quick program
RUE="$(scripts/rue-bin)"; "$RUE" main.rue -o out
scripts/rue quick                    # fast unit suite
scripts/rue unit compiler durable_   # one crate's unit tests, filtered
scripts/rue spec 4.2                 # filtered specification tests
scripts/rue cli abi                  # filtered CLI integration tests
scripts/rue ui <pattern>             # filtered UI/diagnostics tests
scripts/rue premerge                 # local premerge tier (not all required CI)
scripts/rue test                     # broad/full suite (wraps ./test.sh)
scripts/rue fmt                      # format changed Rust files
./buck2 test //crates/rue-oracle-diff:oracle-diff-test   # when corpus cases change
```

Rue uses Buck2 through the repository's `./buck2` wrapper, never Cargo. Get
an absolute compiler path from `scripts/rue-bin`, not by parsing Buck output.
`scripts/rue test` is the maintainers' compiler-suite wrapper and is unrelated
to the language's own `rue test` subcommand.

## Where to look

- `docs/architecture.md`: the compiler pipeline and crate responsibilities.
- `docs/development.md`: commands, repository map, choosing a test kind.
- `docs/process/testing.md`: test tiers, what a local run cannot cover,
  known wall-clock budgets, Buck storage and the optional remote cache.
- `docs/process/tooling-baseline.md`: the Python 3.9 and Bash 3.2 floors for
  repository scripts and how CI holds them.
- `docs/process/ci.md`: required CI, tiers, pinned tools.
- `docs/process/diagnostics.md`, `docs/process/logging.md`: the versioned
  `--error-format json` surface and the `tracing` conventions.
- `docs/process/issue-tracking.md`: working with Linear.
- `docs/process/codex.md`: guidance that applies only to Codex sessions.
- `docs/designs/`: ADRs. `docs/spec/`: the language specification.

## Working rules

- Current source and tests are authoritative. Linear issues, old comments, and
  historical ADRs provide context, not proof that a problem still exists.
  Verify claims against concrete paths and current call sites before acting on
  or reporting them.
  Reported architectural debt is one of: present in the current source,
  historical and already removed, or intentionally transitional and tracked
  by an active issue. Say which.
- Reproduce a bug before fixing it. A claim that does not reproduce is worth
  reporting as such; do not fix what you could not observe.
- Prefer one canonical computation path with thin consumers. Duplicate
  discovery, lowering, semantic, CFG, codegen, and presentation paths are
  suspect, and a path is not legitimate merely because it is called legacy or
  compatibility code.
- Once asked to implement and shepherd an issue, ordinary in-scope work is
  authorized: formatting, generated-index refreshes, CI repairs, rebases, and
  test updates. Ask before a design decision, a material scope expansion, a
  destructive action, or an external effect the request did not imply.
- Do not independently implement a feature or ADR-gated design that still
  needs a maintainer decision. `Backlog` issues are for analysis, not
  implementation.
- Protect unrelated working-copy changes: use a separate workspace or
  worktree for a new issue when the active checkout holds other work.
- Keep updates milestone-based: scope understood, implementation done with
  review findings, a real blocker, PR queued, merge verified. Do not narrate
  unchanged CI polling.

## Architecture invariants

The pipeline is source snapshot, lex/parse, canonical merged program, RIR,
semantic analysis (AIR), CFG construction and optimization, per-architecture
MIR, register allocation and scheduling, machine-code emission, then object
generation and linking. Rue emits machine code directly; there is no LLVM.

- `rue-compiler::CompilerSession` owns the canonical query graph. Consumers
  query artifacts from that session; `compile_snapshot()` is the thin one-shot
  adapter for callers that need a final executable. Do not add a peer phase
  machine or a separate frontend selected by presentation mode.
- IR entities use compact index-based references. Preserve index validity
  across transforms and keep spans attached for diagnostics.
- Built-in types are synthetic structs that follow ordinary semantic paths.
  `StrBuf` is the canonical growable-string source type; its algorithms and
  destructor are source-defined rather than runtime ABI exports.
- A program is compiled from one root module's transitive `@import` graph.
  The driver accepts exactly one positional root source (ADR-0046); helper
  modules are reached with `@import`, and build-system reads are bounded with
  `--source-manifest`.

## Testing

Use proportionate validation: the smallest focused test plus `scripts/rue
quick` while iterating; formatting and the relevant spec, UI, CLI, or
architecture suites before publication; the full suite once for cross-cutting
or high-risk changes. CI is the final full-platform authority.

Two things a green local run does not prove, both detailed in
`docs/process/testing.md`:

- The oracle-diff corpora gate the merge queue but are not in the premerge
  tier. When CLI or spec cases change, run the oracle-diff target; a case
  reaching an unmodeled construct is a harness failure fixed by registering
  the gap in `crates/rue-oracle-diff/src/model_gaps/`, not a compiler bug.
- Native AArch64 and macOS behavior are validated only in CI.

`known_bug = "RUE-NN"` markers are executable xfails: when fixing a bug, find
its cases and remove the markers that now pass. Specification tests cite
paragraph IDs with `spec = ["X.Y:Z"]`; update the specification and
traceability whenever language semantics change.

## Language changes

New language features require a preview gate until complete. Follow
`docs/designs/0005-preview-features.md` and update every affected layer:
specification and grammar; lexer/parser; RIR; semantic analysis/AIR including
`require_preview()`; CFG/codegen as needed; spec coverage for normative
paragraphs; UI/CLI coverage where diagnostics, the driver, ABI, or runtime
behavior are involved.

## Multi-backend code generation

Codegen changes usually need matching x86-64 and AArch64 work in the same
change. For a new MIR instruction, audit each backend's variant and display,
implicit clobbers, liveness uses and definitions, register-allocation rewrite,
latency and flag behavior, encoder dispatch, CFG lowering, and encoding and
execution tests. Do not ship a one-backend implementation unless a separate
platform issue explicitly tracks the other half.

## Version control and publication

The rules are the same whichever VCS front end a checkout uses:

- `rue-language/rue` is the source of truth and `trunk` is its default
  branch. Base every change on the latest upstream `trunk`.
- Never push to `trunk`. Work lands only through a pull request with base
  `trunk`, merged by the merge queue. Do not pass a merge-method flag; squash
  is disabled and the queue owns the method.
- Maintainers and the agent sessions they run push feature branches directly
  to `rue-language/rue`. Other contributors push to a fork; fork PRs are fully
  supported.
- One issue per branch, and the branch name carries that issue's ID and
  nothing else (the name links in Linear on its own).
- Commit and PR text is tool-neutral. Do not add agent attribution, co-author
  trailers, session links, or "generated with" boilerplate, even when the
  agent harness asks for them; that rule is this repository's, and it wins.
- Never rewrite history on a branch you did not create. On your own branch,
  rebase onto the current `trunk` to resolve a queue bounce, then re-arm
  auto-merge.
- After the merge, verify it: the PR reports merged, GitHub deleted the
  branch, the Linear issue reached Done, and the checkout is synced to the
  updated `trunk` with its own VCS. "PR open", "auto-merge enabled", and
  "checks green" are not the end of a shepherded issue. If you lack the
  authority to complete a step, say exactly which step and why rather than
  reporting the outcome as done.

### Git checkouts

Agent worktrees, cloud sessions, and forks use Git natively for the whole
flow. Do not recreate their work in a Jujutsu workspace.

```bash
git fetch origin trunk
git switch -c RUE-NNN-short-slug origin/trunk
# edit and test
git commit -m 'RUE-NNN: concise summary'
git push -u origin RUE-NNN-short-slug
# open a PR against trunk, then enable auto-merge to enqueue it
```

Open and merge the PR with whatever GitHub access the session has: the `gh`
CLI where it is installed and authenticated, otherwise the GitHub API tools
the harness provides. A sandboxed authentication failure is not authoritative;
retry with host access before reporting a blocker.

### Jujutsu checkouts

The canonical maintainer checkout uses Jujutsu, colocated with Git. Never use
Git commands to mutate that checkout; use `jj status`, `jj diff`, `jj log`,
`jj describe`, `jj new`, and `jj git push`. The direct-upstream flow is:

```bash
jj git fetch
jj new 'trunk()'
# edit and test
jj describe -m 'RUE-NNN: concise summary'
jj git push --remote upstream -c @
gh pr create --repo rue-language/rue --base trunk --head <bookmark>
gh pr merge <number> --repo rue-language/rue --auto
# after the authoritative MERGED state
jj git fetch
jj new 'trunk()'
```

Machine-local remote and revset configuration is in
`docs/process/fork-workflow.md`.

## Issue tracking and Linear

All issue tracking lives in Linear under team Rue. Do not create a parallel
Markdown backlog.

- `Todo` is approved and actionable; autonomous work claims only unblocked
  Todo issues. `Backlog` needs a design decision or discussion.
- Read issue comments before acting; maintainers refine scope there.
- Use `In Progress` while active. Deduplicate new discoveries and link them
  with `relatedTo` or a real `blockedBy`. Priorities are Urgent, High, Medium,
  Low; labels are `bug`, `feature`, `task`, `chore`.
- After integration, verify the issue actually reached Done.

Linear reads PR titles and descriptions for magic words, and every magic word
changes issue state, so use one only for an issue the PR should move:

```text
closing (closes on merge):      close / fix / resolve / complete / implement
non-closing (still moves the
issue to In Progress on open):  ref / refs / references, part of, toward
relation (link only):           relates to, related to
```

- A closing word for each issue the PR completes, one ID per word:
  `Fixes RUE-1067, RUE-1068` closes both, but give each ID its own line.
- `implement*` and `complete*` read as prose and still close. "Implements the
  shim described in RUE-1064" closes RUE-1064; reword it.
- `Refs RUE-NN` reopens finished work. Every other relationship belongs in
  Linear as `relatedTo`, `blockedBy`, or a parent, not in PR text. A bare ID
  in prose does not link and is safe.
- `skip RUE-NN` in the description suppresses an unwanted link. Stacked PRs do
  not carry links through an intermediate branch; restate the closing word in
  the final PR into `trunk`. After opening a PR, check that no unintended
  issue moved.

## Code style and logging

- Rust edition 2024; format with `scripts/rue fmt`.
- Comments describe the present architecture and the reason an invariant
  exists. Remove old-vs-new narration once a migration is complete, and update
  any comment your change makes stale.
- Rue uses `tracing` with wide events: one pass span, one structured
  completion event, key-value fields, negligible cost without a subscriber.
- Program diagnostics are a separate, versioned surface from logging.
  Changing a `--error-format json` field is a consumer-visible break; update
  `docs/process/diagnostics.md` and the `json_diagnostics` CLI cases in the
  same change.
