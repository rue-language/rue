---
description: Design a new feature (ADR + bd epic + subtasks)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Task

Design this feature: $ARGUMENTS

## Instructions

This command handles the full design phase for new features, producing an ADR and bd tracking.

Read and follow `docs/process/planning.md` for details.

Key references:
- `docs/process/planning.md` - Planning workflow
- `docs/designs/README.md` - ADR guide
- `docs/designs/0000-template.md` - ADR template

## Workflow

### 1. Understand the Feature

- Clarify requirements from the conversation context
- Research the codebase to understand impact
- Check `bd ready` for related work

### 2. Assess Size

| Small | Large |
|-------|-------|
| 1-3 files, one session | Many files, multiple phases |
| Add `%` operator | Mutable strings |
| Add `else if` syntax | Inout parameters |
| New warning type | Trait system |

### 3. Create Plan

**For small features:**
- Draft a brief implementation plan
- Create a bd issue after approval

**For large features:**
- Create ADR from template (`docs/designs/NNNN-<feature>.md`)
- Define implementation phases (each should fit in one session)
- Determine if preview gating is needed

### 4. Get Approval

Present the plan and wait for user approval before creating bd issues.

### 5. Create Tracking

**After approval only:**

For small features:
```bash
bd create "<title>" -t feature -p 2 --json
```

For large features:
```bash
# Create epic
bd create "<title>" -t epic -p 2 --json

# Create subtasks for each phase
bd create "Phase 1: <desc>" -t task --parent <epic-id> --json
bd create "Phase 2: <desc>" -t task --parent <epic-id> --json
# ...

# If preview gating needed, add to PreviewFeature enum
```

Update ADR with bd issue IDs.

## Output Format

**Before approval:**
```
## Draft Design

**Type:** small/large feature
**Summary:** <what this does>

[For large: ADR written to docs/designs/NNNN-<feature>.md]

<Implementation plan or phase breakdown>

Please review. Say "approved" to create bd issues, or request changes.
```

**After approval:**
```
## Design Complete

**Issue:** bd-XX - <title>
[For large: **Epic:** bd-XX with subtasks bd-YY, bd-ZZ]

Next: `/implement bd-XX`
```

## Important

- Design only - do not write implementation code
- Do NOT create bd issues until user approves
- Infer scope and priority from conversation context
- For preview features, note the gate requirement in the ADR
- Each phase should fit in one context window
