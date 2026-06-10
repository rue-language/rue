---
description: Design a new feature (ADR + Linear epic + sub-issues)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Task

Design this feature: $ARGUMENTS

## Instructions

This command handles the full design phase for new features, producing an ADR and Linear tracking.

Read and follow `docs/process/planning.md` for details.

Key references:
- `docs/process/planning.md` - Planning workflow
- `docs/designs/README.md` - ADR guide
- `docs/designs/0000-template.md` - ADR template

## Workflow

### 1. Understand the Feature

- Clarify requirements from the conversation context
- Research the codebase to understand impact
- Check for related work with the Linear MCP tools (`list_issues` in the Rue team)

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
- Create a Linear issue after approval

**For large features:**
- Create ADR from template (`docs/designs/NNNN-<feature>.md`)
- Define implementation phases (each should fit in one session)
- Determine if preview gating is needed

### 4. Get Approval

Present the plan and wait for user approval before creating Linear issues.

### 5. Create Tracking

**After approval only**, use the Linear MCP tools:

For small features:
- `save_issue` with `team: "Rue"`, the `feature` label, and priority 3 (Medium)

For large features:
- Create the epic: `save_issue` with `team: "Rue"`, the `feature` label, and priority 3 (Medium)
- Create a sub-issue per phase: `save_issue` with `parentId` set to the epic and the `task` label ("Phase 1: <desc>", "Phase 2: <desc>", ...)
- If preview gating needed, add to PreviewFeature enum

Update ADR with Linear issue IDs.

## Output Format

**Before approval:**
```
## Draft Design

**Type:** small/large feature
**Summary:** <what this does>

[For large: ADR written to docs/designs/NNNN-<feature>.md]

<Implementation plan or phase breakdown>

Please review. Say "approved" to create Linear issues, or request changes.
```

**After approval:**
```
## Design Complete

**Issue:** RUE-XX - <title>
[For large: **Epic:** RUE-XX with sub-issues RUE-YY, RUE-ZZ]

Next: `/implement RUE-XX`
```

## Important

- Design only - do not write implementation code
- Do NOT create Linear issues until user approves
- Infer scope and priority from conversation context
- For preview features, note the gate requirement in the ADR
- Each phase should fit in one context window
