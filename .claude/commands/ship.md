---
description: Rebase, review, test, commit, and push for PR
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
---

## Task

Ship the current work: rebase, format, review, test, commit, push, mark the Linear issue Done, and provide PR URL.

## Workflow

### 1. Rebase on trunk

```bash
jj rebase -d trunk
```

If conflicts, resolve them automatically (see `/fix-conflicts` for strategy).

### 2. Format

```bash
./fmt.sh
```

### 3. Code Review + Auto-fix

Review the current changes following `docs/process/code-review.md`:

```bash
jj show --git
```

Check for:
- Correctness and bugs
- Performance issues
- Style and Rust idioms
- Error handling
- Test coverage (spec tests, UI tests, unit tests)
- Specification updates if changing language semantics
- Multi-backend consistency (if touching `rue-codegen`)

**Auto-fix any issues found.** Do not report and wait - just fix them.

### 4. Run Tests

```bash
./test.sh
```

If tests fail, fix the issues and re-run until green.

### 5. Commit

Determine the Linear issue being worked on from:
- Current jj description
- The Linear MCP tools: `list_issues` in the Rue team with state "In Progress"

Then commit:

```bash
jj commit -m "<commit message>"
```

Follow commit message guidelines:
- Imperative mood ("Add feature" not "Added feature")
- Concise first line (50-72 chars)
- Reference the Linear issue: "Fixes RUE-XX"

### 6. Mark the Linear Issue Done

After committing, use the Linear MCP tools: `save_issue` with state "Done".

### 7. Push and Get PR URL

```bash
jj git push -c @-
```

The pushed change is `@-` because `jj commit` creates a new empty working copy.

### 8. Provide PR URL

The `jj git push` output includes a URL for creating a PR. Extract and provide this URL to the user.

**Do NOT use `gh pr create`** - just give the user the URL from the push output so they can create the PR themselves.

## Output

```
## Shipped!

- Rebased on trunk (no conflicts)
- Formatted code
- Code review: fixed 2 issues
  - Added missing error handling in parse_expr
  - Fixed off-by-one in array bounds check
- Tests: all passing
- Committed: "Add array slice syntax (Fixes RUE-42)"
- Marked Done: RUE-42
- Pushed to: feature/array-slices

**Open PR:** https://github.com/owner/repo/compare/feature/array-slices?expand=1
```

## Important

- Auto-fix all review issues, don't just report them
- Run tests AFTER review fixes to test the final state
- Push `@-` not `@` (commit creates new working copy)
- Commit FIRST, then mark the Linear issue Done (Linear isn't stored in the repo)
- Use `--git` flag for all jj diffs
