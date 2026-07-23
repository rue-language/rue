# Rue repository instructions

This is the canonical guidance for coding agents working in Rue. Tool-specific
instruction files should point here rather than copy these rules.

Rue is an early-stage systems programming language implemented in Rust. It uses
Buck2, Jujutsu, Linear, and a fork-based GitHub contribution workflow.

## Working agreement

### Division of labor

- For end-to-end issue implementation, default to one Luna High implementation
  agent when that capability is available. The coordinating agent investigates,
  writes a precise brief, reviews the returned change, integrates it, and owns
  publication and cleanup.
- Add a separate adversarial reviewer only when the change is architectural,
  cross-phase, unsafe, security-sensitive, or otherwise unusually risky.
- Do not delegate routine polling, repository-instruction discovery, or reading
  this file. Do not create multiple implementation agents for the same scope.
- Protect unrelated working-copy changes. Use an isolated workspace for a new
  issue when the active checkout contains unrelated work.

### Communication

Keep updates milestone-based:

1. Scope understood and implementation delegated.
2. Implementation returned, including material review findings.
3. A real test, CI, design, or authority blocker.
4. PR queued.
5. Merge, tracker closure, and cleanup verified.

Do not narrate unchanged CI polling or repeatedly report how many checks remain.
Do not call ordinary compilation time a stall without evidence.

### Evidence and architectural review

- Current source and tests are authoritative. Linear issues, old comments, and
  historical ADRs provide context, not proof that a problem still exists.
- Before reporting architectural debt, classify it as one of:
  - present in the current source;
  - historical and already removed;
  - intentionally transitional and tracked by an active issue.
- Verify source claims with concrete paths and current call sites. Avoid
  legitimizing a path merely because it is called legacy or compatibility code.
- Prefer one canonical computation path with thin consumers. Treat duplicate
  discovery, lowering, semantic, CFG, codegen, and presentation paths as suspect.

### Autonomy and completion

- Once asked to implement and shepherd an issue, ordinary in-scope fixes are
  authorized: formatting, generated-index refreshes, CI repairs, rebases, and
  test updates. Ask only for a design decision, material scope expansion,
  destructive action, unavailable authority, or external impact not implied by
  the request.
- A sandboxed `gh auth status` or network failure is not authoritative on this
  machine. Retry the required read or publication operation with host access
  before reporting an authentication blocker.
- Do not stop at "PR open," "auto-merge enabled," or "checks green" when the
  requested outcome includes shepherding. Verify the merge itself.
- Unless the user narrows the terminal condition, an end-to-end Rue issue ends
  with the PR merged, Linear closed, the source branch deleted, upstream fetched
  with the checkout's native VCS, and a clean working copy based on the updated
  upstream `trunk`.

## Quickstart

These commands cover most repository work and are runnable from anywhere in the
repository:

```bash
scripts/rue build                    # build the compiler and print its path
scripts/rue exec prog.rue            # compile and run a quick program
RUE="$(scripts/rue-bin)"; "$RUE" main.rue -o out
scripts/rue quick                    # fast unit suite
scripts/rue unit compiler durable_   # one compiler unit test
scripts/rue spec 4.2                 # filtered specification tests
scripts/rue cli abi                  # filtered CLI integration tests
scripts/rue test [pattern]           # broad/full suite
scripts/rue fmt                      # format changed Rust files
scripts/rue gc                       # reclaim stale Buck2 artifacts
scripts/rue cache install            # securely install the shared cache config
scripts/rue cache apply --all        # provision current Git/Codex worktrees
```

Use `scripts/rue-bin` to obtain an absolute compiler path. Do not reconstruct
one from Buck output with `--show-output` and `awk`.

Rue uses Buck2 through the repository's `./buck2` wrapper, not Cargo:

```bash
./buck2 build //crates/rue:rue
./buck2 test //...
./buck2 run //crates/rue:rue -- main.rue -o prog
./buck2 run //crates/rue:rue -- --emit <stage> src.rue
```

## Compiler architecture

The canonical flow is:

```text
SourceSnapshot
  -> lex / parse modules
  -> canonical merged program
  -> RIR
  -> semantic analysis / AIR
  -> CFG construction and optimization
  -> architecture-specific MIR
  -> register allocation and scheduling
  -> machine-code emission
  -> object generation and linking
```

`rue-compiler::CompilerSession` owns the canonical query graph. Consumers query
artifacts from that session; `compile_snapshot()` is the thin one-shot adapter
for callers that need a final executable. Do not introduce a peer phase machine
or a separate frontend selected by presentation mode.

Important design properties:

- IR instructions and entities use compact index-based references. Preserve
  index validity across transforms and keep spans attached for diagnostics.
- Each architecture has its own MIR and backend.
- Rue emits machine code directly; there is no LLVM dependency.
- Built-in types are synthetic structs that follow ordinary semantic paths.
  `StrBuf` is the canonical growable-string source type; its algorithms and
  destructor are source-defined rather than runtime ABI exports.
- A program is compiled from its root module's transitive `@import` graph. The
  driver accepts exactly one positional root source (ADR-0046 / RUE-767);
  additional positional `.rue` arguments are refused. Reach helper modules with
  `@import`, and bound build-system reads with `--source-manifest` — never a
  second positional source.

Crate ownership, when locating changes:

| Crate | Responsibility |
| --- | --- |
| `rue` | CLI and filesystem/project loading |
| `rue-compiler` | canonical session and pipeline orchestration |
| `rue-lexer`, `rue-parser`, `rue-rir` | syntax and untyped IR |
| `rue-air`, `rue-cfg` | semantic analysis, typed IR, CFGs, optimization |
| `rue-codegen` | x86-64 and AArch64 lowering and emission |
| `rue-linker` | object generation and linking |
| `rue-allocator`, `rue-runtime`, `rue-runtime-abi` | heap policy, target runtime, and compiler/runtime ABI contract |
| `rue-error`, `rue-span` | diagnostics and source locations |
| `rue-spec`, `rue-ui-tests`, `rue-cli-tests` | language and integration tests |

## Testing strategy

Use proportionate validation:

1. During implementation, run the smallest focused test that exercises the
   change plus `scripts/rue quick`.
2. Before publication, run formatting and the relevant spec, UI, CLI, or
   architecture-specific suites.
3. Run the broad/full suite once when the change is cross-cutting or high-risk.
   CI is the final full-platform authority.

The generated-oracle smoke target has a fixed two-second child-process budget
and can time out under heavy parallel load on the local macOS host. If failures
are timeout-only and unrelated tests are competing for the machine, rerun that
target once in isolation. A clean isolated 64/64 run is sufficient local
evidence; do not repeatedly rerun the entire suite. A semantic disagreement,
compiler error, or isolated timeout remains a real failure.

An unfiltered `scripts/rue test` is host-serialized across Rue worktrees and
runs the opaque spec, UI, CLI, oracle, and reproducibility harnesses one at a
time. Do not bypass that coordination with a direct `buck2 test //...` when a
full local run is intended. Quick, filtered, and targeted checks do not take the
host lock. The optional BuildBuddy action cache uses one private user config and
ignored per-worktree symlinks; see `docs/process/build-cache.md`. Never commit or
print its credential, and do not use `--prefer-remote` while RUE-320 remains
open.

Testing conventions:

- Unit tests belong in the relevant crate.
- Use `CompilerSession` artifact queries for compiler integration tests. Use
  `compile_snapshot()` only when the final executable adapter matters.
- UI tests cover diagnostics, warnings, flags, and presentation behavior.
- CLI tests run the real binary against real files and should cover driver,
  ABI, multi-file, platform, and runtime-I/O bugs.
- `known_bug = "RUE-NN"` and `known_bug_on = [...]` are executable xfail markers.
  When fixing a bug, find its cases and remove markers that now pass.
- Specification tests cite paragraph IDs with `spec = ["X.Y:Z"]`. Update the
  specification and traceability whenever language semantics change.

## Language changes

New language features require a preview gate until complete. Follow
`docs/designs/0005-preview-features.md` and update every affected layer:

1. Specification and grammar.
2. Lexer/parser.
3. RIR.
4. Semantic analysis/AIR, including `require_preview()`.
5. CFG/codegen as needed.
6. Spec coverage for normative paragraphs.
7. UI/CLI coverage where diagnostics, the driver, ABI, or runtime behavior are
   involved.

Do not independently implement a feature or ADR-gated design that still needs a
maintainer decision.

## Multi-backend code generation

Codegen changes usually need matching x86-64 and AArch64 work. For a new MIR
instruction, audit each backend's:

- variant and display representation;
- implicit clobbers;
- liveness uses and definitions;
- register-allocation rewrite;
- latency, scheduling, and flag behavior;
- encoder dispatch and byte/word encoding;
- CFG lowering;
- encoding and execution tests.

Cross-target assembly can be inspected locally, but native AArch64 execution is
validated by Linux ARM64 and macOS CI. Do not intentionally ship a one-backend
implementation unless a separate platform bug is explicitly tracked.

## Version control and GitHub

The canonical maintainer checkout uses Jujutsu. Never use Git commands to mutate
that checkout. Use `jj status`, `jj diff`, `jj log`, `jj describe`, `jj new`,
and `jj commit` there.

A Codex-managed Git worktree may use Git natively for the entire workflow,
including branching, committing, pushing, and opening a PR. Do not recreate its
work in a Jujutsu workspace merely to follow the maintainer workflow. Respect
the same fork topology and publication rules below regardless of which VCS
front end the checkout uses. If the user explicitly requests a particular VCS
workflow, follow that request.

The maintainer fork topology is fixed:

- `upstream` = `rue-language/rue` — source of truth; never push directly.
- `origin` = `steveklabnik/rue` — push feature bookmarks here.
- PR base = `rue-language/rue:trunk`.

Typical maintainer-checkout flow:

```bash
jj git fetch
jj new 'trunk()'
# edit and test
jj describe -m 'RUE-NNN: concise summary'
jj git push -c @
gh pr create --repo rue-language/rue --base trunk \
  --head steveklabnik:<bookmark>
gh pr merge <number> --repo rue-language/rue --auto --squash
# wait for the authoritative MERGED state
jj git fetch
jj new 'trunk()'
```

After merge, verify that GitHub deleted the feature branch and fetch with the
checkout's native VCS so it sees the deletion and updated upstream `trunk`. Do
not push or synchronize `origin/trunk`. See
`docs/process/fork-workflow.md` for machine-local configuration details.

Commit and PR text should be tool-neutral. Do not add agent attribution,
co-author trailers, or "generated with" boilerplate. Reference `RUE-NN`; see
"Linear integration" below for exactly which words close an issue and when to
use them instead of a plain reference.

## Issue tracking

All issue tracking lives in Linear under team Rue (`RUE-NN`). Do not create a
parallel Markdown backlog.

- `Todo`: actionable without a maintainer design decision. Autonomous work may
  claim unblocked Todo issues.
- `Backlog`: requires design or discussion. Analyze when asked, but do not make
  the decision or begin implementation autonomously.
- Read issue comments before acting; maintainers often refine scope there.
- Use `In Progress` while active. New discoveries should be deduplicated and
  linked with `relatedTo` or a real `blockedBy` dependency.
- Priorities are Urgent, High, Medium, and Low. Use `bug`, `feature`, `task`, or
  `chore` labels consistently.
- A PR body must use at most one closing word per issue, chosen per the rules
  below, and only when the issue is actually done. After integration, verify
  the issue actually reached Done; do not rely solely on synchronization.

## Linear integration

Rue's Linear/GitHub integration is *not* the same as GitHub's own closing-issue
behavior. Its magic-word list is wider, and its auto-linking has side effects
that are easy to trigger by accident. This section is the full reference —
read it instead of Linear's docs when writing a branch name, commit, or PR.

Automatic closing is intentional and should stay in use. The failure mode this
section prevents is agents closing issues too early or reopening finished ones
by accident, not automation itself.

### The two vocabularies

These lists are reproduced verbatim from Linear's integration. Nothing here is
paraphrased — match the words exactly.

**Closing words** (move a linked issue to Done on merge):

```
close, closes, closed, closing
fix, fixes, fixed, fixing
resolve, resolves, resolved, resolving
complete, completes, completed, completing
implement, implements, implemented, implementing
linear issue
```

**Non-closing words** (link an issue for context only, no status change):

```
ref, refs, references
part of
related to, relates to
contributes to
toward, towards
```

Use a non-closing word whenever a PR references an issue without finishing it.

### The prose trap: `implement*` and `complete*`

GitHub does not treat `implements` or `completed` as closing keywords, so
agents trained on GitHub conventions reach for them as ordinary descriptive
prose. In Rue's Linear integration, they close the issue. A PR body that says

> Implements the FFI shim described in RUE-1064

closes RUE-1064 on merge, even if that was never the intent — for example, if
RUE-1064 is a multi-PR umbrella issue and this PR only covers one piece of it.

If you mean "this PR is part of the work in RUE-1064" rather than "this PR
finishes RUE-1064," rewrite it with a non-closing word:

> Implements the FFI shim (refs RUE-1064)

### When closing is correct

Use a closing word only when the PR satisfies every acceptance criterion on
the issue, including tests, docs, and any ADR updates the issue names. If any
criterion is still outstanding, use a non-closing word instead and state what
remains in the PR body. Do not close an issue "mostly" and plan to tidy up
after merge — file or use a follow-up issue for the remainder instead.

### One magic word per ID

A single closing word fans out to every issue ID that follows it in the same
sentence:

> Fixes RUE-1067, RUE-1068 and RUE-1069

closes all three. This silently wipes out a sequenced issue chain when only
one issue in the chain is actually done. Give each issue ID its own sentence
and its own word, closing or not, chosen individually:

> Fixes RUE-1067. Refs RUE-1068, RUE-1069 (follow-up work, not yet done).

### Branch and title discipline

An issue ID in a branch name or PR title auto-links the PR, and the team's
on-merge automation moves linked issues to In Progress when the PR opens —
including issues that are already Done. This pulls completed work backward.

- Put at most one issue ID in a branch name, and only the ID of the issue that
  branch completes.
- Do not brand a follow-up branch with a prior, already-completed issue's ID.
  File a new issue for follow-up work instead of reusing the old one.
- Naming a completed issue in a PR title for context has the same reopening
  effect as a branch name; prefer referencing it in the PR body with a
  non-closing word instead.

### `skip` / `ignore`

When an issue ID must appear somewhere in the PR (e.g., an already-completed
issue is unavoidably present in a carried-over branch name), put `skip RUE-NN`
or `ignore RUE-NN` in the PR description. This fully unlinks the PR from that
issue and suppresses all status automation for it, even though the branch name
still contains the ID.

### Placement rules

- Magic words (closing or non-closing) are only recognized in the **PR title**
  and **PR description**. They do nothing in PR comments.
- In commit messages, linking requires a magic word immediately before the
  issue ID — a bare `RUE-NN` in a commit message does not link or close
  anything.

### Squash and stacked-PR caveat

Merging several PRs into an intermediate branch and then merging that branch
onward does **not** carry the original PRs' issue links forward. The final PR
into `trunk` only links whatever issue IDs and magic words appear in its own
title and description. Re-state the closing/non-closing references in the
final PR, even if they were already correct on the PRs merged into it.

## Code style and logging

- Rust edition 2024; format with `scripts/rue fmt`.
- Describe the present architecture and the reason an invariant exists. Remove
  transitional old-vs-new narration once the migration is complete.
- Rue uses `tracing` with wide events: one pass span, one structured completion
  event, key-value fields, and negligible cost without a subscriber. See
  `docs/process/logging.md`.
