# CLAUDE.md

Issue tracking lives in [Linear](https://linear.app) (team: **Rue**, issues `RUE-NN`) — see [Issue Tracking](#issue-tracking-linear). Version control is [Jujutsu](https://jj-vcs.github.io) (`jj`), not git — see [Version Control](#version-control).

## Quickstart

The 9 commands that cover ~90% of work here (all runnable from anywhere in the repo):

```bash
scripts/rue build                    # build the compiler and print its path
scripts/rue exec prog.rue            # compile prog.rue to a temp file AND run it (quick check)
RUE="$(scripts/rue-bin)"; "$RUE" main.rue -o out      # drive the real CLI (@imports resolved from disk)
scripts/rue test [pattern]           # full suite (= ./test.sh): unit + spec + UI + CLI + traceability
scripts/rue quick                    # unit tests only (~2-5s, fast inner loop)
scripts/rue spec 4.2                 # run spec tests matching a pattern
scripts/rue cli abi                  # run CLI integration tests matching a pattern
scripts/rue fmt                      # format (= ./fmt.sh) before committing
scripts/rue gc                       # reclaim disk (= buck2 clean --stale 1w)
```

To get the compiler binary, use `scripts/rue-bin` (prints an absolute path; the old `buck2 ... --show-output | awk` one-liner returns a *relative* path that breaks when cwd changes).

## Project Overview

Rue is a systems programming language aiming for memory safety without garbage collection, with higher-level ergonomics than Rust/Zig. Currently in early development with Rust-like syntax.

## Build System

Buck2 via the `./buck2` wrapper (NOT Cargo). The Quickstart commands above cover
most work; the long-form invocations they wrap:

```bash
./buck2 build //crates/rue:rue          # build the compiler
./buck2 test //...                      # all unit + suite tests (sh_test targets)
./buck2 run //crates/rue:rue -- main.rue -o prog       # real CLI
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
| `rue-codegen` | x86-64 and aarch64 machine code generation |
| `rue-linker` | ELF/Mach-O object file creation and linking |
| `rue-error` | Error types and diagnostics |
| `rue-span` | Source location tracking |
| `rue-target` | Target platform configuration |
| `rue-spec` | Specification test runner |
| `rue-ui-tests` | UI/diagnostics tests (warnings, error messages) |
| `rue-cli-tests` | CLI integration tests (real binary, real files) |
| `rue-fuzz` | Fuzz testing infrastructure |
| `rue-runtime` | Runtime support |
| `rue-builtins` | Built-in type definitions (String) |

### Modules and Multi-File Compilation

Rue compiles a program from its **root module's import graph**: pass the root
file and imports are discovered transitively from disk.

```bash
rue main.rue -o program        # @imports resolved and loaded automatically
```

**Key semantics:**
- `const util = @import("util.rue");` binds a module object; access members as
  `util.helper()`. Directory modules use an internal facade file (`app/_app.rue`
  is the module file for `app/`).
- `@import("std")` loads the standard library bundle (`std.math`,
  `std.option.Option`, `std.arraybuf.ArrayBuf`). There is no prelude (ADR-0042).
- Privacy is uniform (spec 10.3:7): an item — function, struct, enum, or
  constant — is usable outside its defining directory only if `pub` (E0460
  otherwise; through a module object it's E0706).
- `--source-manifest <path>` restricts which files imports may read — for build
  systems (Buck) that need the compiler's input set to be explicit.
- `main()` must exist in exactly one file.
- Files are parsed sequentially with a shared interner (so interned symbols
  agree across files), then merged for semantic analysis.

**Legacy flat mode is being removed (ADR-0046):** extra positional source files
(`rue a.rue b.rue -o prog`) are still accepted as legacy inputs, but unqualified
cross-file name resolution is gone — referencing another file's items without
`@import` is an undefined-name error (E0202). Functions have module-qualified
internal identity (RUE-426/RUE-441, closed 2026-07-11): sibling modules may
define the same top-level name — including same-name generics and same-named
structs' methods — without colliding (regression cases:
`crates/rue-cli-tests/cases/sibling_module_identity.toml`). Don't write new
tests or examples that rely on flat file lists.

### Key Design Decisions

- **Architecture-specific MIR**: Each target gets its own machine IR (currently X86Mir), following Zig's approach
- **Index-based references**: Instructions stored in vectors, referenced by u32 indices (cache-friendly, no lifetimes)
- **Direct code emission**: No LLVM dependency; machine code emitted directly
- **Minimal linking**: Static executables with direct syscalls
- **Built-in types as synthetic structs**: Types like `String` are defined in `rue-builtins` and injected as synthetic structs, not as hardcoded special-cased built-in types (see [ADR-0020](docs/designs/0020-builtin-types-as-structs.md))

### Built-in Types Architecture

Built-in types (currently `String`) are "synthetic structs" injected before user
code, so they flow through the same paths as user structs (see
[ADR-0020](docs/designs/0020-builtin-types-as-structs.md)). Collection types
like `ArrayBuf` are NOT builtins — they are ordinary Rue source in `std/`,
imported via `@import("std")` (see ADR-0043). To add a new builtin: define a
`BuiltinTypeDef` in `rue-builtins/src/lib.rs`, add it to `BUILTIN_TYPES`,
implement runtime functions in `rue-runtime`. The module docs in `rue-builtins`
walk through a full worked example. Injection point: `inject_builtin_types()`
in `rue-air/src/sema/builtins.rs`.

## Testing

### Development Workflow

The test suite has three layers optimized for different stages of development:

| Test Type | Command | Speed | When to Use |
|-----------|---------|-------|-------------|
| Unit tests | `./quick-test.sh` | ~2-5s | During active development |
| Full suite | `./test.sh` | ~30-60s | Before committing |
| Targeted spec | `./buck2 run //crates/rue-spec:rue-spec -- "pattern"` | Varies | Testing specific features |

**Rule of thumb:**
- **Unit tests** catch logic errors quickly during development
- **Spec tests** verify language semantics and maintain spec traceability
- **UI tests** verify compiler quality-of-life features (warnings, error messages)
- **CLI tests** catch driver/ABI/multi-file bugs the spec harness can't see
- About to commit? Run `./test.sh`.

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
./buck2 run //crates/rue-cli-tests:rue-cli-tests -- "abi"   # filter by pattern
```

Key conventions (see the doc comment in `crates/rue-cli-tests/src/main.rs` for the full case format):

- Each case lists `files` written to disk; the default invocation is `rue <first file> -o prog` with the temp dir as cwd
- Any compiler panic is reported as an **INTERNAL COMPILER ERROR** — a distinct failure class
- `known_bug = "RUE-NN"` marks an expected failure (xfail) referencing a Linear issue. The case still runs; if it unexpectedly PASSES, the suite fails and tells you to remove the marker — converting it into a regression test. **When fixing a bug, find and un-mark its cases.**
- `known_bug_on = ["x86-64-linux"]` scopes the xfail to specific platforms; platform names match `get_host_target()`: `x86-64-linux`, `aarch64-linux`, `aarch64-macos`
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

Use preview gating when adding new syntax (keywords, operators, constructs), new type system features, or any feature spanning multiple implementation phases.

#### How to Gate a Feature

1. **Add to PreviewFeature enum** in `rue-error/src/lib.rs`; also update `name()`, `adr()`, `all()`, and the `FromStr` impl.

2. **Add the gate check in Sema** (`rue-air/src/sema/analysis.rs`):
   ```rust
   self.require_preview(PreviewFeature::YourNewFeature, "your feature description", span)?;
   ```
   **This is the critical step that actually gates the feature!** Without this call, users can use the feature without `--preview`.

3. **Add spec tests with `preview` field** (`preview = "your_new_feature"`, matching `PreviewFeature::name()`).

4. **Test that the gate works**: without `--preview your_new_feature` you get a "requires preview feature" error; with it, the feature compiles/runs.

#### Stabilizing a Feature

When all tests pass and the feature is complete: remove `preview = "..."` from spec tests, remove the `require_preview()` call, remove the enum variant, and update the ADR status to "Implemented".

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

7. **Add spec tests** in `crates/rue-spec/cases/` with `spec = ["X.Y:Z"]` references covering all normative paragraphs (traceability enforces 100%); include `preview = "..."` for preview features

8. **Add UI tests** in `crates/rue-ui-tests/cases/` for new warnings/lints, error message formatting changes, or new compiler flags

9. **Run `./test.sh`** to verify all tests pass and traceability is maintained

## Codegen: Multi-Backend Considerations

**IMPORTANT**: The `rue-codegen` crate contains multiple architecture backends:
- `x86_64/` - Linux x86-64
- `aarch64/` - macOS/Linux ARM64

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

**IMPORTANT**: This project uses **Jujutsu (jj)**, NOT git. Never use git commands to mutate this repository.

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

### Contributing changes

Base work on the latest `trunk`, make your change, push it as a branch, and open
a PR against `rue-language/rue` with base `trunk`. Never commit directly on
`trunk` — that causes hash-rewrite divergence when upstream rebase/squash-merges.

If you work from a **fork** (separate `origin`/`upstream` remotes), see
[docs/process/fork-workflow.md](docs/process/fork-workflow.md) for the remote
configuration, revset aliases, and push/PR flow.

### Commit Messages

Use `jj commit -m "message"`, or for multi-line messages:
```bash
jj commit -m "Short summary

Longer description here.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

Reference issue IDs (RUE-NN) in commit messages.

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

## Issue Tracking (Linear)

ALL issue tracking lives in **Linear**, team **Rue** (`RUE-NN`). Use the Linear
MCP tools (`list_issues`, `get_issue`, `save_issue`, `save_comment`, …). Do not
create markdown TODO lists or parallel tracking systems.

### States

The dividing line is **whether starting the work needs a human design decision**:

- **`Todo`** = actionable **and** needs no design decision to start: bugs,
  well-specified chores, infrastructure, CI, refactors. The only state
  autonomous work pulls from. Skip issues blocked by open issues.
- **`Backlog`** = needs a human design decision or discussion first: features
  (any new language capability), ADR-gated work, design docs — off-limits to
  autonomous work *even when technically actionable*. It is the maintainers'
  shared design-discussion venue: read it, contribute analysis in comments when
  asked, but never action or decide a Backlog item on your own.
- Design direction (from any maintainer) often lives in issue **comments**, not
  the description — read them before acting on or re-stating any issue.

### Workflow

1. Claim: `save_issue` with `state: "In Progress"`, `assignee: "me"`, then
   `jj describe -m "WIP: RUE-42 - short description"`
2. Implement, test, document
3. Discovered work → new issue with `relatedTo` (or `blockedBy` for a true
   dependency) pointing at the issue you were working on. Multi-phase features
   get a parent epic + sub-issues via `parentId`, with the ADR linked.
4. Complete via the PR (below) — no manual state change needed.

**Priorities**: `1` Urgent (security, broken builds), `2` High, `3` Medium
(default), `4` Low. **Labels**: `bug`, `feature`, `task`, `chore`.

### Closing issues: GitHub ↔ Linear sync

A merged PR auto-links and auto-closes the issues its **body** references:

- Put `Fixes RUE-NN` (or `Closes`/`Resolves`) in the PR body — **one issue per
  line**; a comma list only closes the first. Branch names don't carry issue
  IDs, so body keywords are what counts.
- A non-closing reference ("Part of RUE-NN") moves the issue to `In Progress`
  but never out — the **stranded-In-Progress hazard**. After each integration
  round, sweep `In Progress`: anything without an active worker goes back to
  `Todo` (work remains) or `Done` (it doesn't). The sync can also flip a
  manually-closed issue back when an older PR attaches — re-close it.
- Marking `Done` manually is an optional backstop for PRs the sync missed.
