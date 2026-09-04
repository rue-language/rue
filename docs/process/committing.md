# Committing Changes

This document describes how we create commits.

## Prerequisites

Before committing:
1. All tests pass (`./test.sh`)
2. Code review is complete (see [code-review.md](code-review.md))
3. No blocking issues remain

## Version Control

The maintainers' canonical checkout uses **Jujutsu (jj)**; agent worktrees,
cloud sessions, and forks use Git. The publication rules are the same for
both and live in [AGENTS.md](../../AGENTS.md#version-control-and-publication).
With jj, the working copy is always a commit (no staging area): `jj commit`
finalizes it and starts a new change, and `jj status` / `jj diff` show the
current state.

## Commit Message Guidelines

### Format

```
<summary line>

<optional body>

<optional footer>
```

### Summary Line

- Use imperative mood ("Add feature" not "Added feature")
- Keep to 50 characters or less (hard limit: 72)
- Capitalize first letter
- No period at the end

**Good**: `Add modulo operator`
**Bad**: `Added the modulo operator.`

### Body (Optional)

- Separate from summary with blank line
- Wrap at 72 characters
- Explain **what** and **why**, not **how** (code shows how)
- Provide context for future readers

### Footer (Optional)

- Reference Linear issues: `Fixes RUE-42` or `Related to RUE-42`
- Multiple issues on separate lines
- No agent attribution, co-author trailers, or session links; commit text is
  tool-neutral (see [AGENTS.md](../../AGENTS.md#version-control-and-publication))

### Examples

**Simple change:**
```
Fix off-by-one error in bounds checking
```

**With context:**
```
Add modulo operator (%)

Implements the modulo operator for integer types. The operator
follows Rust semantics: the result has the same sign as the
dividend (truncated division).

Fixes RUE-42
```

**Multi-issue:**
```
Refactor type checking for binary operators

Consolidates duplicate type-checking logic for arithmetic,
comparison, and bitwise operators into a shared helper function.
This prepares for adding new operators without code duplication.

Related to RUE-45
Related to RUE-46
```

## Workflow

### 1. Create the Commit

```bash
jj commit -m "<message>"      # Jujutsu checkout
git commit -m "<message>"     # Git checkout
```

For multi-line messages, omit `-m` and use your editor.

### 2. Mark Related Issues Done

If this commit completes a Linear issue, mark it Done **after** committing: use the Linear MCP `save_issue` tool with state "Done". Linear state isn't stored in the repo, so the commit message reference (`Fixes RUE-NN`) is what links the two.

### 3. Verify

After committing, the working copy should be clean (`jj status` or
`git status`), and `jj log -r @-` or `git log -1` shows the commit you just
made.

## What NOT to Include

- **File lists**: The VCS shows what changed
- **Obvious descriptions**: "Update foo.rs" adds no value
- **WIP markers**: Don't commit work-in-progress
- **Temporary changes**: Debug prints, commented code

## Commit Atomicity

Each commit should:
- **Be complete**: Tests pass, feature works (or is properly gated)
- **Be focused**: One logical change per commit
- **Be reviewable**: Someone can understand it in isolation

If you have multiple unrelated changes, make multiple commits.

## Special Cases

### Preview Feature Work

When committing partial work on a large feature:
- Tests may be added with `preview = "..."` flag
- Stable tests must still pass
- Commit message should note the phase: "Add parsing for inout parameters (phase 1)"

### Stabilization Commits

When removing a preview gate:
```
Stabilize modulo operator

Remove preview gate and mark feature as stable. All tests pass
without the preview flag.

Closes RUE-42 (epic)
```

### Spec-Only Changes

When updating documentation without code:
```
Document array indexing semantics

Add specification paragraphs for array bounds checking behavior
and update spec tests with traceability references.
```
