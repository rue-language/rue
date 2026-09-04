# Rue repository instructions

This is the canonical guidance for coding agents working in Rue. Tool-specific
instruction files should point here rather than copy these rules.

Rue is an early-stage systems programming language implemented in Rust. It uses
Buck2, Jujutsu, Linear, and a pull-request-based GitHub contribution workflow.

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
scripts/rue premerge                 # local premerge test tier (not all required CI)
scripts/rue slow                     # canonical scheduled slow tier
scripts/rue stress                   # canonical opt-in stress tier
scripts/rue all                      # canonical union of all tiers
scripts/rue unit compiler durable_   # one compiler unit test
scripts/rue spec 4.2                 # filtered specification tests
scripts/rue cli abi                  # filtered CLI integration tests
scripts/rue test [pattern]           # broad/full suite (the maintainers' compiler-suite
                                     # wrapper — unrelated to the language's own
                                     # `rue test` subcommand, docs/process/test-events.md)
scripts/rue fmt                      # format changed Rust files
scripts/rue storage status           # inventory Buck disk use across worktrees
scripts/rue storage plan             # dry-run stale cleanup in every registered worktree
scripts/rue storage clean            # reclaim stale Buck2 artifacts host-wide
scripts/rue storage reset RUE_ROOT   # full Buck reset of one registered worktree
scripts/rue cache install            # securely install the private cache config
```

When corpus-affecting cases change, use this focused check:
`./buck2 test //crates/rue-oracle-diff:oracle-diff-test`.

Use `scripts/rue-bin` to obtain an absolute compiler path. Do not reconstruct
one from Buck output with `--show-output` and `awk`.

Rue uses Buck2 through the repository's `./buck2` wrapper, not Cargo:

```bash
./buck2 build //crates/rue:rue
./buck2 test //...
./buck2 run //crates/rue:rue -- main.rue -o prog
./buck2 run //crates/rue:rue -- --emit <stage> src.rue
```

## Repository tooling baseline

The repository's Python tooling requires Python 3.9 or newer, uniformly —
nothing in the tree needs more. The floor was briefly 3.11 because
`scripts/cli-timeout-policy.py` and its tests read the CLI execution contracts
with `tomllib`, stdlib only from 3.11 (RUE-1509); since RUE-1524 those
contracts are consumed as a Buck-materialized JSON twin, derived at build time
via `//crates/rue-toml2json`, and the floor is a uniform 3.9 again.

A stock Mac now meets the floor: macOS ships `/usr/bin/python3` as 3.9.6, and
3.9 is chosen precisely so that interpreter is enough — nothing to install.
The runners' own interpreters are comfortably above the floor —
`ubuntu-latest` and `ubuntu-24.04-arm` provide 3.12.3, `macos-15` provides
3.14.6 — so a premerge-tier target using a construct newer than 3.9 would stay
green on them and fail on a stock developer machine, which is what
`//:cli-timeout-policy-validation` did while it needed 3.11 (RUE-1509). CI
therefore holds the floor by running the tooling under it: the `fmt`,
`linux-premerge`, and `ci-contract` jobs install Python 3.9 with
`actions/setup-python` before any gate runs, so a construct newer than the
floor fails there the way it fails on a stock Mac (RUE-1936 retired the static
scanner that approximated this with a curated table of constructs).

This floor governs the interpreter that runs repository tooling. It is not the
Python number in `docs/process/build-cache.md`, which records what the pinned
remote worker image ships for the Buck prelude's rustc wrapper — a different
interpreter running different code. The remote-execution canary builds; it does
not run these tests.

Shell has the same shape of floor and a stricter one. macOS ships GNU Bash
3.2.57 as `/bin/bash` and will not ship a GPLv3 one, so a `#!/usr/bin/env bash`
script has to run on 3.2 — on a stock Mac and on a `macos-*` runner that is the
interpreter it gets. `scripts/validate-shell-bash-baseline.py` holds two checks
to that floor, and neither covers the other. A curated construct table names
Bash 4+ spellings, which is what catches a script that parses on 3.2 and then
misbehaves: `mapfile` exiting 127 (RUE-1506), `${v:1:-1}` silently answering
empty. A `bash -n` pass parses every discovered shell script — `#!/bin/sh` and
bare `.sh` included, since a syntax error is one in any shell — and that is
what catches a file which does not parse at all: RUE-1511 shipped an unbalanced
double quote inside a multi-line command substitution, and the table called it
clean because a syntax error is not a construct (RUE-1512).

The two halves need different interpreters and get them in different places.
An unbalanced quote is a syntax error on every bash, so the Linux `fmt` job's
run catches that class on every pull request. `;;&`, `;&`, and `coproc` are a
syntax error only on 3.2, so that half is authoritative on the `macos-15` leg
of `native-platforms`, which runs the gate with `--require-baseline-bash` and
fails if its `/bin/bash` is not a 3.x. Every run says which interpreter parsed
and whether that was the baseline, so a weaker run cannot read as a stronger
one, and an empty discovery set fails rather than passing as a clean tree.

Annotate a reviewed table exception with `# bash-baseline-ok: <reason>`. The
parse check has its own, `# bash-parse-ok: <reason>`, which is file-level
because a parse failure names where the parser gave up rather than where the
mistake is. It exists for one real case: `bash -n` parses without executing, so
it never runs a `shopt`, and a file enabling `extglob` and then using `@(a|b)`
runs correctly on 3.2.57 while `bash -n` on that same interpreter rejects it.
A genuine syntax error is fixed, not annotated.

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
| `rue-perf-schema` | compiler-performance measurement contract (ADR-0067) |
| `rue-bench` | compiler-performance measurement runner (ADR-0067) |

## Testing strategy

Use proportionate validation:

1. During implementation, run the smallest focused test that exercises the
   change plus `scripts/rue quick`.
2. Before publication, run formatting and the relevant spec, UI, CLI, or
   architecture-specific suites.
3. Run the broad/full suite once when the change is cross-cutting or high-risk.
   CI is the final full-platform authority.

A tier is not a preview of required CI, which is why the quickstart names the
oracle-diff check separately. `test.sh` selects on `rue_test_tier_premerge`,
while both oracle-diff corpora are `tier = "slow"` and
`test (linux-x64-oracle-diff*)` gates the merge queue regardless — so no local
premerge run can report on them, however green it comes back.

Adding a CLI or spec case is the ordinary way to land in that gap, not an edge
case: the corpus audit classifies every case, and one reaching a construct the
oracle does not model is a HARNESS FAILURE until its gap is registered in
`crates/rue-oracle-diff/src/model_gaps/`. A `StrBuf`-built path reaches the
unmodeled byte-copy gap immediately, so a new `std.fs` case starts there by
default. The audit prints the exact `Entry::new(...)` line to add, which is
the whole fix — read past "unknown oracle model gap" to it rather than reading
the message as a compiler bug. RUE-1711 cost a red CI round for want of this.

The generated-oracle smoke target has a fixed two-second child-process budget
and can time out under heavy parallel load on the local macOS host. If failures
are timeout-only and unrelated tests are competing for the machine, rerun that
target once in isolation. A clean isolated 64/64 run is sufficient local
evidence; do not repeatedly rerun the entire suite. A semantic disagreement,
compiler error, or isolated timeout remains a real failure.

One other wall-clock budget is deliberate: the linking test
`platform_native_system_link_cancellation_reaps_child_and_cleans_workspace`
gives cancellation five seconds to answer, measured from the cancel request
rather than from the start of the link, because its mock linker would otherwise
live for thirty seconds and the point of the test is that cancellation does not
wait the child out (RUE-2005). Every other wait in that module synchronizes on
an event and its bound is only a hang guard.

An unfiltered `scripts/rue test` delegates selection and execution to Buck in
one `buck2 test` invocation. Its standard selection includes the premerge and
slow tiers while leaving stress tests opt-in. Corpus suites are cacheable Buck
build actions; `weight_percentage = 100` keeps two corpus actions from running
together within one Buck daemon/worktree while still allowing corpus work to
overlap non-corpus actions such as unit tests and compiles. There is no
full-suite host lock or cross-worktree test serialization, so sibling worktrees
may run concurrently. There is no cross-worktree coordination: the `buck2`
wrapper only refuses to start a build below a 4 GiB free-space floor, and a
finished worktree's `buck-out` is reclaimed by removing the worktree. The
optional BuildBuddy action cache is one private user config
(`scripts/rue cache install`) that the wrapper links as `.buckconfig.local` in
a worktree on the first build there; `RUE_NO_REMOTE_CACHE=1` skips that for
one command. See `docs/process/build-cache.md`. Never commit or print its
credential. Full remote execution is supported (RUE-320, Done 2026-07-18): the
repository wrapper defaults to `--prefer-local`, and `--prefer-remote` is an
explicit opt-in for cache-population or RE-debugging runs, not the default
local-development policy.

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
the same base and publication rules below regardless of which VCS front end the
checkout uses. If the user explicitly requests a particular VCS workflow,
follow that request.

The publication topology is:

- `upstream` = `rue-language/rue` — source of truth. Steve and Dorian should
  prefer pushing feature branches here when access and tooling allow it. Never
  push directly to `trunk`.
- Other contributors push feature branches to their `origin` fork. Fork-based
  PRs remain fully supported.
- PR base = `rue-language/rue:trunk`.

Typical direct-upstream maintainer flow for Steve and Dorian:

```bash
jj git fetch
jj new 'trunk()'
# edit and test
jj describe -m 'RUE-NNN: concise summary'
jj git push --remote upstream -c @
gh pr create --repo rue-language/rue --base trunk --head <bookmark>
gh pr merge <number> --repo rue-language/rue --auto
# wait for the authoritative MERGED state
jj git fetch
jj new 'trunk()'
```

`trunk` is behind a merge queue, so `--auto` enqueues the PR and the queue
performs the merge once it reaches the front and merge-group CI passes. Do not
pass a merge-method flag: squash is disabled on the repository (`--squash`
fails with a 405), and the queue owns the method regardless. A direct
`gh pr merge` without `--auto` is refused by branch protection with
"Changes must be made through the merge queue".

For a fork-based contribution, push with `--remote origin` and pass
`--head <user>:<bookmark>` when creating the PR.

After merge, verify that GitHub deleted the feature branch and fetch with the
checkout's native VCS so it sees the deletion and updated upstream `trunk`. Do
not push or synchronize a fork's `origin/trunk`. See
`docs/process/fork-workflow.md` for machine-local configuration details.

Commit and PR text should be tool-neutral. Do not add agent attribution,
co-author trailers, or "generated with" boilerplate. Name the issue the change
belongs to as `RUE-NN`; see "Linear integration" below, because every magic
word — including `refs` — changes issue state.

## Issue tracking

All issue tracking lives in Linear under team Rue (`RUE-NN`). Do not create a
parallel Markdown backlog.

- `Todo`: approved and actionable without a maintainer design decision. A Todo
  issue may still wait on an explicit blocker; autonomous work may claim only
  unblocked Todo issues.
- `Backlog`: requires design or discussion. Analyze when asked, but do not make
  the decision or begin implementation autonomously.
- Read issue comments before acting; maintainers often refine scope there.
- Use `In Progress` while active. New discoveries should be deduplicated and
  linked with `relatedTo` or a real `blockedBy` dependency.
- Priorities are Urgent, High, Medium, and Low. Use `bug`, `feature`, `task`, or
  `chore` labels consistently.
- Relationships between issues are recorded here, never by citing an ID in a
  PR — see "Linear integration". After integration, verify the issue actually
  reached Done; do not rely solely on synchronization.

## Linear integration

A magic word is a command that changes issue state, not a citation. Use one
only for an issue the PR should actually move — a closing word for each issue
it completes, once that issue's acceptance criteria are met. Every other
relationship belongs in Linear as `relatedTo`, `blockedBy`, or a parent, not in
PR text.

The trap: "non-closing" does not mean read-only. It skips only the on-merge
status, and the default automation still moves a linked issue to In Progress
when the PR opens — including issues already Done or Canceled. `Refs RUE-NN`
reopens finished work; PR #2318 did exactly that to two issues on 2026-08-12.
Word classes per [Linear's GitHub docs](https://linear.app/docs/github):

```
closing (full automation, closes on merge):
    close / fix / resolve / complete / implement, any tense; "linear issue"
non-closing (skips only the merge status, still moves the issue):
    ref / refs / references, part of, contributes to, toward / towards
relation (links only, no status change):
    relates to, related to
```

- `implement*` and `complete*` are the easy accident: GitHub ignores them, so
  they get written as prose, but Linear closes on them. "Implements the shim
  described in RUE-1064" closes RUE-1064 — reword rather than reach for `refs`.
- A word applies to every ID that follows it, so `Fixes RUE-1067, RUE-1068`
  closes both. Give each ID its own word.
- A bare ID does not link, so naming an issue in prose is safe. The branch name
  does link on its own; keep one ID there, the issue that branch completes.
- `skip RUE-NN` or `ignore RUE-NN` in the description suppresses an unwanted
  link.
- Magic words work in the PR title and description only, not in comments;
  commit messages need the word immediately before the ID.
- Stacked PRs don't carry links through an intermediate branch — restate the
  closing word in the final PR into `trunk`.
- After opening a PR, check that no unintended issue moved.

## Code style and logging

- Rust edition 2024; format with `scripts/rue fmt`.
- Describe the present architecture and the reason an invariant exists. Remove
  transitional old-vs-new narration once the migration is complete.
- Rue uses `tracing` with wide events: one pass span, one structured completion
  event, key-value fields, and negligible cost without a subscriber. See
  `docs/process/logging.md`.
- Program diagnostics are a separate, versioned surface from logging.
  `--error-format json` publishes them as structured JSON on stderr; the schema
  and its ICE/ordering guarantees are in `docs/process/diagnostics.md`. Changing
  a JSON field there is a consumer-visible break — update the doc and the
  `json_diagnostics` CLI cases in the same change.
