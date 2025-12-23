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

1. **Clarify requirements** - Make sure you understand what's being asked
2. **Research the codebase** - Use Glob/Grep/Read to understand relevant code
3. **Check for existing work** - Run `bd ready --json` to see if already tracked

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

### Step 3: Create the Plan

**For small features:**

1. Create a bd issue with clear scope:
   ```bash
   bd create "<feature title>" -t feature -p 2 --json
   ```

2. Present a brief implementation plan to the user:
   - Which files need changes
   - What spec sections need updates (if any)
   - What tests need to be added
   - Estimated complexity

**For large features:**

1. Create a bd issue:
   ```bash
   bd create "<feature title>" -t epic -p 2 --json
   ```

2. Draft an ADR in `docs/designs/adr-NNN-<feature>.md`:
   - Use the next available ADR number
   - Include clear implementation phases
   - Each phase should be independently committable
   - Reference ADR-020 for preview feature pattern

3. Add the feature to `PreviewFeature` enum in `crates/rue-error/src/lib.rs`

4. Present the ADR to the user for approval

### Step 4: Get Approval

Present your plan and ask the user:
- Does this scope look correct?
- For large features: Do the phases make sense?
- Any adjustments needed before implementation?

**Do not proceed to implementation.** Tell the user to run `/implement <bd-id>` when ready.

## Output Format

Your output should end with something like:

```
## Plan Complete

**Issue:** bd-XX - <title>
**Type:** small/large feature
**Files to modify:** <list>

Ready to implement? Run: `/implement bd-XX`
```

## Important Notes

- This command is **planning only** - do not write implementation code
- Keep context usage low - don't read entire codebase
- For large features, phases should be small enough to complete in one context window
- The user may want to adjust the plan before implementing
