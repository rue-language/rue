---
description: Plan a new feature (outputs ADR or bd issue)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Context

You are working on the Rue programming language compiler. Review the project structure and CLAUDE.md for context.

## Your Task

Plan this feature: $ARGUMENTS

This command is for **planning only**. It produces either:
- A bd issue (for small features)
- An ADR document + bd issue (for large features)

Use `/implement` to execute the plan once approved.

## Workflow

### Step 1: Understand the Feature

1. **Clarify requirements** - Ask the user clarifying questions to fully understand the feature
2. **Research the codebase** - Use Glob/Grep/Read to understand relevant code
3. **Check for existing work** - Run `bd ready --json` to see if already tracked

**Important:** Do NOT create any bd issues yet. We need to agree on the plan first.

### Step 2: Assess Feature Size

**Small Feature** (bd issue only):
- Touches 1-3 files
- Single concept (new operator, syntax sugar, simple addition)
- No new runtime functions
- No new IR instruction kinds
- Completable in one focused session

**Large Feature** (requires ADR + preview gate):
- Touches many files across multiple crates
- Multiple implementation phases
- New runtime support functions
- New IR instruction kinds
- New type system concepts
- Could span multiple sessions

Examples:
| Small | Large |
|-------|-------|
| Add `%` operator | Mutable strings |
| Add `else if` syntax | Inout parameters |
| Add unary `+` | Enums and pattern matching |
| New warning type | Trait system |

### Step 3: Draft the Plan

**For small features:**

Present a brief implementation plan to the user:
- Which files need changes
- What spec sections need updates (if any)
- What tests need to be added
- Estimated complexity

**For large features:**

1. Draft an ADR in `docs/designs/adr-NNN-<feature>.md`:
   - Use the next available ADR number
   - Include clear implementation phases
   - Each phase should be independently committable
   - Reference ADR-020 for preview feature pattern

2. Present the ADR to the user for review

**Do NOT create bd issues yet.** The plan must be approved first.

### Step 4: Iterate on the Plan

Work with the user to refine the plan:
- Answer questions about the approach
- Adjust phases or scope as needed
- Update the ADR based on feedback

Continue iterating until the user explicitly approves the plan.

### Step 5: Create Issues (After Approval Only)

**Only after the user approves the plan**, create the bd issues:

**For small features:**
```bash
bd create "<feature title>" -t feature -p 2 --json
```

**For large features:**

1. Create the epic:
   ```bash
   bd create "<feature title>" -t epic -p 2 --json
   ```

2. Create a subtask for each implementation phase from the ADR:
   ```bash
   bd create "Phase 1: <description>" -t task --parent <epic-id> --json
   bd create "Phase 2: <description>" -t task --parent <epic-id> --json
   # ... etc
   ```

3. Add the feature to `PreviewFeature` enum in `crates/rue-error/src/lib.rs`

## Output Format

**Before approval**, your output should be:

```
## Draft Plan

**Type:** small/large feature
**Files to modify:** <list>

[For large features: ADR has been written to docs/designs/adr-NNN-<feature>.md]

Please review the plan. Let me know if you'd like any changes, or say "approved" to create the bd issues.
```

**After approval**, your output should be:

```
## Plan Complete

**Epic:** bd-XX - <title>
**Subtasks:**
- bd-YY - Phase 1: <description>
- bd-ZZ - Phase 2: <description>
- ...

Ready to implement? Run: `/implement bd-YY` (or start with any subtask)
```

## Important Notes

- This command is **planning only** - do not write implementation code
- **Do NOT create bd issues until the user explicitly approves the plan**
- Keep context usage low - don't read entire codebase
- For large features, phases should be small enough to complete in one context window
- Iterate on the ADR with the user until they're satisfied
