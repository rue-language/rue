# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Note**: This project uses [Linear](https://linear.app) (team: **Rue**) for issue tracking. Use the Linear MCP tools instead of markdown TODOs. See the Issue Tracking section below for workflow details.

## Quickstart

The 9 commands that cover ~90% of work here (all runnable from anywhere in the repo):

```bash
scripts/rue build                    # build the compiler and print its path
scripts/rue exec prog.rue            # compile prog.rue to a temp file AND run it (quick check)
RUE="$(scripts/rue-bin)"; "$RUE" a.rue b.rue -o out   # drive the real CLI (modules, multi-file)
scripts/rue test [pattern]           # full suite (= ./test.sh): unit + spec + UI + CLI + traceability
scripts/rue quick                    # unit tests only (~2-5s, fast inner loop)
scripts/rue spec 4.2                 # run spec tests matching a pattern
scripts/rue cli abi                  # run CLI integration tests matching a pattern
scripts/rue fmt                      # format (= ./fmt.sh) before committing
scripts/rue gc                       # reclaim disk (= buck2 clean --stale 1w)
```

Key rules (details in the sections below):
- **Version control is `jj`, not git**, and this is a **fork** — never commit on `trunk`; work on a change and `jj git push -c @-` to PR a feature branch upstream. See [Version Control](#version-control).
- **Issue tracking is Linear** (team Rue, `RUE-NN`). See [Issue Tracking](#issue-tracking-with-linear).
- To get the compiler binary, use `scripts/rue-bin` (prints an absolute path; the old `buck2 ... --show-output | awk` one-liner returns a *relative* path that breaks when cwd changes).

## Project Overview

Rue is a systems programming language aiming for memory safety without garbage collection, with higher-level ergonomics than Rust/Zig. Currently in early development with Rust-like syntax.

## Build System

Buck2 via the `./buck2` wrapper (NOT Cargo). The Quickstart commands above cover
most work; the long-form invocations they wrap:

```bash
./buck2 build //crates/rue:rue          # build the compiler
./buck2 test //...                      # all unit + suite tests (sh_test targets)
./buck2 run //crates/rue:rue -- a.rue b.rue -o prog   # real CLI (-o required for multi-file)
./buck2 run //crates/rue:rue -- --emit <stage> src.rue # tokens|ast|rir|air|cfg|mir|asm (repeatable)
```

## Architecture

The compiler pipeline transforms source through successive IRs:

```mermaid
graph LR
    Source --> Lexer --> Parser --> AstGen --> Sema --> CfgBuilder --> Lower --> RegAlloc --> Emit --> Link
```

| Stage | Pass | IR Produced | `--emit` flag |
|-------|------|-------------|---------------|
| 1 | Lexer | tokens | `tokens` |
| 2 | Parser | AST | `ast` |
| 3 | AstGen | RIR (untyped) | `rir` |
| 4 | Sema | AIR (typed) | `air` |
| 5 | CfgBuilder | CFG | `cfg` |
| 6 | Lower | MIR (machine) | `mir` |
| 7 | RegAlloc | MIR (allocated) | `asm` |
| 8 | Emit | bytes | - |
| 9 | Link | ELF | - |

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| `rue` | CLI binary |
| `rue-compiler` | Pipeline orchestration |
| `rue-lexer` | Tokenization |
| `rue-parser` | AST construction |
| `rue-rir` | Untyped IR (post-parse, pre-typing) |
| `rue-cfg` | Control flow graph construction and optimization |
| `rue-air` | Typed IR (after semantic analysis) |
| `rue-codegen` | x86-64 machine code generation |
| `rue-linker` | ELF object file creation and linking |
| `rue-error` | Error types and diagnostics |
| `rue-span` | Source location tracking |
| `rue-target` | Target platform configuration |
| `rue-spec` | Specification test runner |
| `rue-ui-tests` | UI/diagnostics tests (warnings, error messages) |
| `rue-fuzz` | Fuzz testing infrastructure |
| `rue-runtime` | Runtime support |
| `rue-builtins` | Built-in type definitions (String, future Vec, etc.) |

### Multi-File Compilation

Rue supports compiling multiple source files into a single executable:

```bash
# Explicitly listed files share a flat global namespace (transitional, spec 10.5:2)
rue main.rue utils.rue lib.rue -o program
```

**Key semantics:**
- All top-level names resolve across files without imports (flat namespace),
  but privacy is uniform (spec 10.3:7): an item — function, struct, enum, or
  constant — is usable outside its defining directory only if `pub`, whether
  its file is imported or just listed (E0460 otherwise; through a module
  object it's E0706)
- Duplicate definitions (same name in multiple files) cause a compile error
- `main()` must exist in exactly one file
- Files are parsed in parallel, then merged for semantic analysis

**Current limitations (transitional, see spec 10.5:2):**
- Top-level names are not yet module-scoped — all symbols share one global
  scope, so names collide program-wide and `pub` items remain callable
  unqualified across files
- No `mod` or `use` syntax (`@import` + `pub const` re-exports instead)

### Key Design Decisions

- **Architecture-specific MIR**: Each target gets its own machine IR (currently X86Mir), following Zig's approach
- **Index-based references**: Instructions stored in vectors, referenced by u32 indices (cache-friendly, no lifetimes)
- **Direct code emission**: No LLVM dependency; machine code emitted directly
- **Minimal ELF**: Static executables with direct syscalls (Linux x86-64 only)
- **Built-in types as synthetic structs**: Types like `String` are defined in `rue-builtins` and injected as synthetic structs, not as hardcoded `Type` enum variants (see [ADR-0020](docs/designs/0020-builtin-types-as-structs.md))

### Built-in Types Architecture

Built-in types (`String`, future `Vec<T>`) are "synthetic structs" injected
before user code, so they flow through the same paths as user structs
(see [ADR-0020](docs/designs/0020-builtin-types-as-structs.md)). To add one:
define a `BuiltinTypeDef` in `rue-builtins/src/lib.rs`, add it to
`BUILTIN_TYPES`, implement runtime functions in `rue-runtime`. The module docs
in `rue-builtins` walk through a full hypothetical `Vec` example. Injection
point: `inject_builtin_types()` in `rue-air/src/sema.rs`.

## Testing

### Development Workflow

The test suite has three layers optimized for different stages of development:

| Test Type | Command | Speed | When to Use |
|-----------|---------|-------|-------------|
| Unit tests | `./quick-test.sh` | ~2-5s | During active development |
| Full suite | `./test.sh` | ~30-60s | Before committing |
| Targeted spec | `./buck2 run //crates/rue-spec:rue-spec -- "pattern"` | Varies | Testing specific features |

**Recommended workflow:**

```bash
# During development - fast feedback loop
./quick-test.sh                # Unit tests only

# Before committing - full verification
./test.sh                      # Unit + spec + UI + traceability

# Debugging specific areas
./buck2 run //crates/rue-spec:rue-spec -- "arithmetic"  # Specific spec tests
./buck2 test //crates/rue-codegen:rue-codegen-test      # Specific crate
```

### Choosing the Right Test Type

| If you're... | Use... | Why |
|--------------|--------|-----|
| Iterating on a fix | `./quick-test.sh` | Fast feedback, catches most issues |
| Adding a language feature | Spec tests | Required for traceability |
| Improving diagnostics | UI tests | Not spec-mandated behavior |
| About to commit | `./test.sh` | Ensures nothing is broken |

**Rule of thumb:**
- **Unit tests** catch logic errors quickly during development
- **Spec tests** verify language semantics and maintain spec traceability
- **UI tests** verify compiler quality-of-life features (warnings, error messages)

### Unit Tests
Add to relevant crate's source file with `#[cfg(test)]` modules. Ensure crate has `rust_test` target in its `BUCK` file.

The `rue-compiler` crate includes integration unit tests that test the full pipeline without execution. Use `compile_to_air()` and `compile_to_cfg()` helpers to test compilation without spawning processes.


### UI Tests

UI tests (`crates/rue-ui-tests/cases/`) verify compiler behavior **not** in the
spec: warnings, diagnostic quality, flags, message wording. Spec tests carry
`spec = [...]` traceability references; UI tests don't. Full TOML format
reference: `crates/rue-ui-tests/README.md`.

```bash
./buck2 run //crates/rue-ui-tests:rue-ui-tests -- "pattern"
```

### CLI Integration Tests

CLI integration tests (`crates/rue-cli-tests/cases/`) exercise the compiler **the way a user does**: the real `rue` binary invoked on real files in a temp directory with relative paths, env vars, and stdin piped to the compiled program. They catch driver-only bugs the spec harness can't see (module resolution from disk, ABI miscompilations, ICEs, CLI argument handling).

```bash
# Run all CLI integration tests
./buck2 run //crates/rue-cli-tests:rue-cli-tests

# Filter by pattern
./buck2 run //crates/rue-cli-tests:rue-cli-tests -- "abi"
```

Key conventions (see the doc comment in `crates/rue-cli-tests/src/main.rs` for the full case format):

- Each case lists `files` written to disk; the default invocation is `rue <first file> -o prog` with the temp dir as cwd
- Any compiler panic is reported as an **INTERNAL COMPILER ERROR** — a distinct failure class
- `known_bug = "RUE-NN"` marks an expected failure (xfail) referencing a Linear issue. The case still runs; if it unexpectedly PASSES, the suite fails and tells you to remove the marker — converting it into a regression test. **When fixing a bug, find and un-mark its cases.**
- `known_bug_on = ["x86-64-linux"]` scopes the xfail to specific platforms (for ABI bugs that manifest differently per target); on other platforms the case runs as a normal test. Platform names match `get_host_target()`: `x86-64-linux`, `aarch64-linux`, `aarch64-macos`
- Prefer adding a CLI case (not just a spec test) for any bug that involves the driver, the ABI, multiple files, or runtime I/O


### Specification Tests

Spec tests (`crates/rue-spec/cases/`, organized by feature area) link tests to
spec paragraphs via `spec = ["X.Y:Z"]` (chapter.section:paragraph). The
traceability check in `./test.sh` enforces 100% coverage of normative
paragraphs and rejects dangling references. The spec source lives in
`docs/spec/src/` with `{{ rule(id="X.Y:Z", cat="category") }}` paragraph
markers; categories `normative`/`legality-rule`/`dynamic-semantics`/`syntax`/
`undefined-behavior` are normative and need test coverage.

Full format reference (case TOML, golden tests, preview-feature tests, spec
authoring, traceability reports): `crates/rue-spec/README.md`.

```bash
./buck2 run //crates/rue-spec:rue-spec -- "4.2"             # filter by pattern
./buck2 run //crates/rue-spec:rue-spec -- --traceability    # coverage report
```

### Fuzz Testing

Mutation + property-based fuzzing over lexer/parser/sema/compiler/emitter
targets, run daily in CI (`.github/workflows/fuzz.yml`); crashes land in
`crates/rue-fuzz/crashes/`. Full documentation: `crates/rue-fuzz/README.md`.

## Modifying the Language

When adding or changing language features, follow this checklist.

### Preview Features (Gating New Features)

**IMPORTANT**: New language features MUST be gated behind preview flags until complete. See [ADR-0005](docs/designs/0005-preview-features.md) for the full design.

#### When to Use Preview Features

Use preview gating when:
- Adding new syntax (keywords, operators, constructs)
- Adding new type system features
- Any feature that spans multiple implementation phases

#### How to Gate a Feature

1. **Add to PreviewFeature enum** in `rue-error/src/lib.rs`:
   ```rust
   pub enum PreviewFeature {
       YourNewFeature,  // Add your feature here
   }
   ```
   Also update `name()`, `adr()`, `all()`, and `FromStr` impl.

2. **Add the gate check in Sema** (`rue-air/src/sema.rs`):
   ```rust
   // At the point where the feature is used:
   self.require_preview(PreviewFeature::YourNewFeature, "your feature description", span)?;
   ```

   **This is the critical step that actually gates the feature!** Without this call, users can use the feature without `--preview`.

3. **Add spec tests with `preview` field**:
   ```toml
   [[case]]
   name = "your_feature_basic"
   spec = ["X.Y:Z"]
   preview = "your_new_feature"  # Matches PreviewFeature::name()
   source = """..."""
   exit_code = 42
   ```

4. **Test that the gate works**:
   - Without `--preview your_new_feature`: Should get "requires preview feature" error
   - With `--preview your_new_feature`: Should compile/run

#### Stabilizing a Feature

When all tests pass and the feature is complete:

1. Remove `preview = "..."` from spec tests
2. Remove the `require_preview()` call from Sema
3. Remove the variant from `PreviewFeature` enum
4. Update the ADR status to "Implemented"

### Implementation Steps

1. **Update the specification** (`docs/spec/src/`)
   - Add/modify spec paragraphs with proper IDs (e.g., `r[4.2:3#normative]`)
   - Include normative rules, dynamic semantics, and examples
   - Update the grammar appendix if syntax changes

2. **Update `rue-lexer`** if new tokens needed

3. **Update `rue-parser`** for new syntax

4. **Update `rue-rir`** for new IR instructions

5. **Update `rue-air`** for typed versions
   - **If this is a new feature**: Add the `require_preview()` gate (see above)

6. **Update `rue-codegen`** for code generation

7. **Add spec tests** in `crates/rue-spec/cases/`
   - Include `spec = ["X.Y:Z"]` references to link to spec paragraphs
   - Cover all normative paragraphs (traceability check enforces 100% coverage)
   - **If this is a preview feature**: Include `preview = "feature_name"` field

8. **Add UI tests** in `crates/rue-ui-tests/cases/` if the feature includes:
   - New warnings or lints
   - Changes to error message formatting
   - New compiler flags or options

9. **Run `./test.sh`** to verify all tests pass and traceability is maintained

## Codegen: Multi-Backend Considerations

**IMPORTANT**: The `rue-codegen` crate contains multiple architecture backends:
- `x86_64/` - Linux x86-64
- `aarch64/` - macOS ARM64

When making changes to codegen, **always check if the same change is needed in all backends**. Common areas that require parallel changes:

- **New MIR instructions**: Add to both `x86_64/mir.rs` and `aarch64/mir.rs`
- **Instruction emission**: Update both `x86_64/emit.rs` and `aarch64/emit.rs`
- **Register allocation**: Update both `x86_64/regalloc.rs` and `aarch64/regalloc.rs`
- **Liveness analysis**: Update both `x86_64/liveness.rs` and `aarch64/liveness.rs`
- **CFG lowering**: Update both `x86_64/cfg_lower.rs` and `aarch64/cfg_lower.rs`

### Checklist: adding a new MIR instruction variant

Adding a variant (e.g. a 64-bit `Cqo`, `And64RR`, or aarch64 `Sdiv64RR`) touches the **same set of match arms** in each backend. Miss one and you get a non-exhaustive-`match` build error (or, worse, a silently-wrong reg-alloc). For **each** backend (`x86_64/` and `aarch64/`), update:

1. **`mir.rs`** — three places: the `enum` variant definition; the `Display` impl (`write!`); and `clobbers()` if it has implicit register effects (e.g. div clobbers rax/rdx).
2. **`liveness.rs`** — two match arms: register **uses** (operands read) and **defs** (operands written). Add the variant alongside its closest sibling.
3. **`regalloc.rs`** — the instruction-rewrite match (rebuild the variant with allocated operands; reuse `emit_binop`/`emit_ternop`/`load_operand` like the sibling).
4. **`schedule.rs`** — usually four arms: `get_latency`, `regs_read`, `regs_written`, and `writes_flags` (if it sets flags).
5. **`emit.rs`** — the encoder dispatch arm (calls a helper + `end_inst!`) and the encoder helper itself (the actual byte/word encoding). Add a unit test pinning the bytes.
6. **`cfg_lower.rs`** — select the new variant where appropriate (this is the actual fix).

Tip: `grep -n <SiblingVariant> crates/rue-codegen/src/<arch>/*.rs` lists every site you need to mirror.

**Testing across backends**: x86-64 is verifiable locally (build + run). aarch64 is **not runnable locally** — you can only `--emit asm` and read it (cross-target *links* are refused outright because only the host runtime is embedded; RUE-36 / ADR-0034). But CI's `test (linux-arm64)` and `test (macos)` jobs build `rue` *natively* on arm64 and run the binaries, so an aarch64-only bug **will fail CI**. Therefore: **apply the parallel aarch64 change in the SAME PR** — never ship an x86-only fix expecting to follow up, or CI will bounce it. Use `known_bug` / `known_bug_on` in CLI tests only when a *separate, tracked* bug makes a case fail on one platform.

## Version Control

**IMPORTANT**: This project uses **Jujutsu (jj)**, NOT git. Never use git commands in this repository.

### Common jj Commands

| Instead of git... | Use jj... |
|-------------------|-----------|
| `git status` | `jj status` |
| `git diff` | `jj diff` |
| `git add . && git commit -m "msg"` | `jj commit -m "msg"` (auto-adds all changes) |
| `git log` | `jj log` |
| `git checkout -b branch` | `jj new -m "description"` |
| `git stash` | Not needed - jj auto-saves working changes |

### Key Differences from git

- **No staging area**: `jj commit` automatically includes all changes
- **Working copy is a commit**: Your uncommitted changes are already tracked
- **Use `jj describe`** to update the current commit message
- **Use `jj new`** to start a new change on top of current one

### Fork Workflow (IMPORTANT)

This is a **fork** setup. There are two git remotes:

- `upstream` = `rue-language/rue` — the canonical repo, the source of truth. You **cannot** push here; you open PRs into it.
- `origin` = `steveklabnik/rue` — your fork. You push feature branches here, then PR them upstream.

**Rules:**

1. **Always base work on `trunk()` (= `trunk@upstream`); do NOT push or sync `origin/trunk`.** You never need to mirror `origin/trunk` to upstream — cross-fork PRs diff against `upstream/trunk`, and jj's immutability anchor is `trunk@upstream`. So `origin/trunk` may sit stale (behind upstream); that's harmless. `trunk@origin` is untracked (see required config) precisely so you aren't tempted to push it. Never commit on `trunk` / PR `trunk` — that causes hash-rewrite divergence when upstream rebase/squash-merges.
2. **Work on a feature change**, then push it as a branch and PR it:
   ```bash
   jj new 'trunk()'                # start the change on upstream's canonical trunk (a revset, not a bookmark)
   # ... make edits ...
   jj git push -c @                # pushes as steveklabnik/push-<changeid> (see git_push_bookmark template)
   gh pr create --repo rue-language/rue --base trunk --head steveklabnik:<branch> ...
   gh pr merge <n> --repo rue-language/rue --auto   # queue it immediately
   ```
3. **`trunk()` is a revset alias = `trunk@upstream`** — always means upstream's latest, regardless of local bookmark state. Always use `trunk()`, never the bare `trunk` bookmark, in `jj new`/rebase/log commands.
4. **After a PR merges**, the only step is: `jj git fetch` (your local `trunk` fast-forwards to upstream), then `jj new 'trunk()'` to start the next change. Do **not** push `trunk` to origin — there's nothing to sync. If upstream rebase-merged (rewriting hashes), the old fork-side copies show as "divergent" — cosmetic; `jj abandon` the orphaned old-hash chain to tidy up.

**Required repo config** (machine-local; set on a fresh clone — jj does not read committed config):

```bash
jj config set --repo 'revset-aliases."trunk()"' 'trunk@upstream'   # base/immutability = canonical repo
jj config set --repo git.fetch '["origin", "upstream"]'            # always see both remotes
jj bookmark untrack 'trunk' --remote=origin                        # don't track/sync origin/trunk; base on upstream only
```

Without the first two, `jj git fetch` only pulls `origin` (you won't see upstream merges), and `trunk()`/immutability anchor to your fork instead of upstream. The `untrack` keeps the local `trunk` bookmark tracking *only* `upstream`, so it fast-forwards to upstream on fetch and you never feel obligated to push it back to origin.

### Commit Messages

When committing, use `jj commit -m "message"` or for multi-line messages:
```bash
jj commit -m "Short summary

Longer description here.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

## Code Style

- Standard Rust formatting (rustfmt)
- Rust edition 2024

## Logging

Rue uses `tracing` with a "wide events" philosophy: one `info_span!()` per
compilation pass (this powers `--time-passes`), one structured `info!()` event
with metrics on completion, key-value fields over interpolated strings, and
zero cost when no subscriber is active. `--log-level=debug`,
`--log-format=json`, and `RUST_LOG` module filters are supported.
Full guidelines and examples: `docs/process/logging.md`.

## Issue Tracking with Linear

**IMPORTANT**: This project uses **Linear** for ALL issue tracking, in the **Rue** team. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Access

In Claude Code, use the Linear MCP tools (`list_issues`, `get_issue`, `save_issue`, `save_comment`, `list_my_issues`, etc.). Issues are identified as `RUE-NN`.

### Quick Start

- **Find ready work**: list issues in the Rue team with state `Todo`, ordered by priority; skip issues blocked by open issues
- **State semantics** — the dividing line is **whether starting the work needs a human design decision**, not merely whether it's actionable (refined 2026-07-02 from the original 2026-06-11 "actionable vs. do-not-action" ruling):
  - **`Todo`** = actionable **and** needs no design decision to start: bugs, well-specified chores, infrastructure, CI, refactors. This is the only state autonomous work pulls from.
  - **`Backlog`** = needs a human design decision or discussion first: **features** (any new language capability), ADR-gated work, design docs — off-limits to autonomous work *even when technically "actionable."* Backlog is the **shared design-discussion venue** for the maintainers (Steve, Dorian): read it, and contribute analysis in comments when asked, but never action or decide a Backlog item on your own.
  - New syntax, design rulings, and ADR ratifications are **filed** to Backlog, never decided autonomously.
  - Watch for Linear comments from any maintainer (not just Steve) on Backlog issues — that's where design gets hashed out.
- **Create an issue**: `save_issue` with `team: "Rue"`, a clear title, and a Markdown description
- **Claim**: `save_issue` with `state: "In Progress"` and `assignee: "me"`
- **Complete**: `save_issue` with `state: "Done"`

### Conventions

- **Multi-phase features**: create a parent issue (the "epic") and sub-issues per phase via `parentId`; link the ADR in the description
- **Discovered work**: when you find new work mid-task, create a new issue with `relatedTo` (or `blockedBy` if it's a true dependency) pointing at the issue you were working on
- **Priorities (Linear semantics)**: `1` Urgent (security, broken builds), `2` High (major features, important bugs), `3` Medium (default), `4` Low (polish, backlog ideas)
- **Labels**: use `bug`, `feature`, `task`, `chore` to mirror issue types

### Workflow for AI Agents

1. **Check ready work**: list `Todo` issues in the Rue team (`Backlog` is off-limits — see state semantics above)
2. **Claim your task**: set state to `In Progress`
3. **Describe the working commit**: `jj describe -m "WIP: RUE-42 - short description"`
   - This makes it easy to see what's being worked on in each workspace
   - Will be overwritten with the final commit message when complete
4. **Work on it**: Implement, test, document
5. **Discover new work?** Create a linked issue (`relatedTo` the current one)
6. **Complete**: put `Fixes RUE-42` in the **PR body** — the issue moves to `Done` automatically when the PR merges (see below). No manual state change is needed.

### Closing issues: GitHub ↔ Linear sync

The Linear ↔ GitHub integration is connected to both `steveklabnik/rue` (the fork) and `rue-language/rue` (upstream). A merged PR **auto-links and auto-closes** the issues it references, so you don't need to set issues to `Done` by hand.

- Put a closing keyword in the **PR body** (not just the commit message): `Fixes RUE-NN` (also accepts `Closes`/`Resolves`). The PR body is what the integration parses.
- **One issue per line** for a PR that fixes several issues — `Fixes RUE-28` / `Fixes RUE-60` / `Fixes RUE-98`, each on its own line. A bare comma list (`Fixes RUE-28, RUE-60`) only closes the first.
- The branch name (`steveklabnik/push-<changeid>`) does **not** carry the issue ID, so rely on the PR-body keywords, not branch-name linking. This also handles multi-issue PRs, which a branch name can't.
- On merge, the integration links the PR as an attachment on each issue **and** transitions it to `Done`. Marking `Done` manually is an optional backstop (e.g. for a PR that merged before the integration existed, which it won't retroactively process).
- **Stranded `In Progress` hazard**: the integration moves an issue to `In Progress` when any PR/branch references it, but only closing keywords transition it out — a "Part of RUE-NN" PR strands the issue in `In Progress` forever. After each integration round, sweep `In Progress`: anything without an active worker goes back to `Todo` (work remains) or `Done` (it doesn't). The integration can also flip a manually-closed issue back to `In Progress` when an older PR attaches — re-close it.

### Important Rules

- ✅ Use Linear for ALL task tracking
- ✅ Link discovered work to the issue it came from
- ✅ Reference issue IDs (RUE-NN) in commit messages
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use other issue trackers
- ❌ Do NOT duplicate tracking systems
