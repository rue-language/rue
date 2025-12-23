---
description: Plan and implement a new feature (convenience wrapper)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Context

You are working on the Rue programming language compiler.

## Your Task

Plan and implement this feature: $ARGUMENTS

## Recommended Workflow

This command combines planning and implementation. For better context management, consider using the split commands:

1. **`/plan <description>`** - Creates a plan (ADR or bd issue)
2. **`/implement <bd-id>`** - Implements a planned feature

The split approach is better for:
- Large features that span multiple sessions
- When you want to review the plan before implementing
- When context window limits are a concern

## Combined Workflow

If proceeding with combined planning + implementation:

### Step 1: Plan (see /plan for details)

1. Understand and research the feature
2. Assess size (small vs large)
3. Create bd issue (and ADR for large features)
4. Present plan to user for approval

### Step 2: Get Approval

Before implementing, confirm with the user:
- Does the scope look correct?
- Ready to proceed with implementation?

### Step 3: Implement (see /implement for details)

1. Update specification if needed
2. Add tests first
3. Make code changes
4. Run `./test.sh`
5. Run `/code-review`
6. Run `/commit`

## Size Guidelines

**Small Feature** (no preview gate):
- 1-3 files, single concept, one session

**Large Feature** (requires ADR + preview gate):
- Many files, multiple phases, may span sessions

| Small | Large |
|-------|-------|
| Add `%` operator | Mutable strings |
| Add `else if` syntax | Inout parameters |
| New warning type | Trait system |

## Important

- For large features, prefer `/plan` then `/implement` separately
- Each commit should leave tests passing
- Always run `/code-review` before `/commit`
