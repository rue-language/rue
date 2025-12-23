# Committing Changes

This document describes how we create commits. The `/commit` command automates this workflow.

## Prerequisites

Before committing:
1. All tests pass (`./test.sh`)
2. Code review is complete (`/code-review`)
3. No blocking issues remain

## Version Control

We use **Jujutsu (jj)**, not git. Key differences:
- Working copy is always a commit (no staging area)
- Use `jj commit` to finalize and start a new change
- Use `jj status` and `jj diff` to see current state

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

- Reference bd issues: `Fixes bd-42` or `Related to bd-42`
- Multiple issues on separate lines

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

Fixes bd-42
```

**Multi-issue:**
```
Refactor type checking for binary operators

Consolidates duplicate type-checking logic for arithmetic,
comparison, and bitwise operators into a shared helper function.
This prepares for adding new operators without code duplication.

Related to bd-45
Related to bd-46
```

## Workflow

### 1. Close Related Issues

If this commit completes a bd issue, close it **before** committing:

```bash
bd close <bd-id> --reason "Completed"
```

This ensures the issue closure is included in the same commit as the code changes.

### 2. Create the Commit

```bash
jj commit -m "<message>"
```

For multi-line messages, use your editor:
```bash
jj commit
```

### 3. Verify

After committing, the working copy becomes a new empty change. You can verify with:
```bash
jj log -r @-   # See the commit you just made
jj status      # Should show clean working copy
```

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

Closes bd-42 (epic)
```

### Spec-Only Changes

When updating documentation without code:
```
Document array indexing semantics

Add specification paragraphs for array bounds checking behavior
and update spec tests with traceability references.
```
