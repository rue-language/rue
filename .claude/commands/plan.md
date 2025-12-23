---
description: Plan a new feature (outputs ADR or bd issue)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Task

Plan this feature: $ARGUMENTS

## Instructions

Read and follow `docs/process/planning.md` for the full planning workflow.

Key references:
- `docs/process/planning.md` - Planning workflow
- `docs/designs/README.md` - ADR guide
- `docs/designs/0000-template.md` - ADR template

## Summary

1. **Understand** - Clarify requirements, research codebase, check `bd ready`
2. **Assess size** - Small (1-3 files, one session) vs Large (many files, phases)
3. **Create plan**:
   - Small: Draft a brief implementation plan
   - Large: Create ADR from template (`docs/designs/NNNN-<feature>.md`)
4. **Get approval** - Present plan, iterate until approved
5. **Finalize** - Create bd issues only after approval:
   - Small: Single bd issue
   - Large: bd epic + subtasks, add to PreviewFeature enum

## Output

**Before approval:**
```
## Draft Plan

**Type:** small/large feature
**Summary:** <what this does>

[For large: ADR written to docs/designs/NNNN-<feature>.md]

Please review. Say "approved" to create bd issues, or request changes.
```

**After approval:**
```
## Plan Complete

**Issue:** bd-XX - <title>
[For large: **Epic:** bd-XX with subtasks bd-YY, bd-ZZ]

Next: `/implement bd-XX`
```

## Important

- Planning only - do not write implementation code
- Do NOT create bd issues until user approves the plan
- For large features, each phase should fit in one context window
