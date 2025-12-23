---
description: Implement a planned feature from a bd issue
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <bd-id>
---

## Context

You are working on the Rue programming language compiler. Review the project structure and CLAUDE.md for context.

## Your Task

Implement the feature tracked by: $ARGUMENTS

This command takes a bd issue ID and implements it. Use `/plan` first if you need to create a plan.

## Workflow

### Step 1: Load the Issue

1. Get the issue details:
   ```bash
   bd show $ARGUMENTS --json
   ```

2. If this is an epic (large feature), check for an ADR:
   - Look in `docs/designs/` for the referenced ADR
   - Identify which phase you're implementing
   - If no phase is specified, start with phase 1

3. Mark the issue as in progress:
   ```bash
   bd update $ARGUMENTS --status in_progress --json
   ```

### Step 2: Scope Check

Before implementing, verify the work fits in the current context:

**Good scope (proceed):**
- Clear, bounded changes
- 1-5 files to modify
- Single logical unit of work

**Too large (split it):**
- More than 5-7 files
- Multiple unrelated changes
- Would require extensive exploration

If too large, create subtasks:
```bash
bd create "Subtask: <description>" --parent $ARGUMENTS -t task --json
```
Then implement subtasks one at a time.

### Step 3: Implement

**For small features / subtasks:**

1. **Update specification** (if changing language semantics)
   - Add/modify paragraphs in `docs/spec/src/`
   - Use proper paragraph IDs: `r[X.Y:Z#category]`

2. **Add tests first**
   - Spec tests in `crates/rue-spec/cases/` with `spec = ["X.Y:Z"]`
   - UI tests in `crates/rue-ui-tests/cases/` for warnings/diagnostics
   - Unit tests for internal implementation details (see below)

3. **Make code changes**
   - If touching `rue-codegen`: implement in ALL backends (x86_64 and aarch64)
   - Follow existing patterns in the codebase

4. **Verify**
   ```bash
   ./test.sh
   ```

**For large features (with preview gate):**

1. **Add tests with preview flag**
   - Use `preview = "<feature_name>"` in test cases
   - These tests run but are allowed to fail

2. **Add semantic gates**
   - Use `require_preview()` in sema for new syntax/operations
   - Check `preview_features` in CompileOptions

3. **Implement the phase**
   - Make incremental progress
   - Each commit should leave stable tests passing

4. **Verify**
   ```bash
   ./test.sh
   ```
   - Stable tests must pass
   - Preview tests for this phase should pass when complete

### Step 4: Review and Commit

1. **Run code review:**
   ```
   /code-review
   ```

2. **Fix any blocking issues** found in review

3. **Commit the changes:**
   ```
   /commit
   ```

   The `/commit` command will:
   - Close the bd issue before committing
   - Create an appropriate commit message

### Step 5: Next Steps

**If this was a subtask of a larger feature:**
- Check if more subtasks remain
- Tell user what's next: `/implement bd-XX` for next subtask

**If this completes a large feature:**
1. Remove `preview = "..."` from spec tests
2. Remove `require_preview()` gates from sema
3. Remove feature from `PreviewFeature` enum
4. Update ADR status to "Stable"
5. Create final commit for stabilization

## Unit Tests

Add unit tests when they make sense for internal implementation details that aren't
covered by spec or UI tests. Good candidates:

- **Data structure methods**: Parsing helpers, conversion functions, lookup tables
- **Error formatting**: Ensure error messages render correctly
- **Internal algorithms**: Register allocation decisions, liveness analysis, IR transforms
- **Edge cases**: Boundary conditions that are hard to trigger via end-to-end tests

Unit tests go in `#[cfg(test)]` modules within the relevant crate. Ensure the crate
has a `rust_test` target in its `BUCK` file.

**When NOT to add unit tests:**
- Behavior already covered by spec tests (avoid duplication)
- Simple passthrough functions
- Code that's trivially correct by inspection

## Important Reminders

- This project uses Buck2 (`./buck2`), not Cargo
- Use `jj` for version control, not git
- Always run `/code-review` before `/commit`
- Each commit should leave tests passing
- Don't bite off more than fits in one context window - split if needed
