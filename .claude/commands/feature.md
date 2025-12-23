---
description: Plan and implement a new feature (convenience wrapper)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Task

Plan and implement this feature: $ARGUMENTS

## Instructions

This command combines `/plan` and `/implement`. For better context management on large features, use them separately.

Read the process documentation:
- `docs/process/README.md` - Overall workflow
- `docs/process/planning.md` - Planning details
- `docs/process/implementation.md` - Implementation details

## Workflow

1. **Plan** - Follow `docs/process/planning.md`:
   - Understand the feature
   - Assess size (small vs large)
   - Draft plan (bd issue, or ADR + epic for large)
   - Get user approval

2. **Implement** - Follow `docs/process/implementation.md`:
   - Update spec if needed
   - Add tests first
   - Make code changes
   - Run `./test.sh`

3. **Finish**:
   - `/code-review`
   - `/commit`

## Size Guidelines

| Small | Large |
|-------|-------|
| 1-3 files, one session | Many files, multiple phases |
| Add `%` operator | Mutable strings |
| Add `else if` syntax | Inout parameters |
| New warning type | Trait system |

## Important

- For large features, prefer `/plan` then `/implement` separately
- Each commit should leave tests passing
- Always run `/code-review` before `/commit`
