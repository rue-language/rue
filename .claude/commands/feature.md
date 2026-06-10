---
description: Plan and implement a new feature (use /design + /implement instead)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Deprecated

This command is deprecated. Use the new workflow instead:

1. **`/design`** - Design the feature (creates ADR + Linear issues)
2. **`/implement <RUE-id>`** - Implement a Linear issue
3. **`/ship`** - Rebase, review, test, commit, push

## Legacy Behavior

If you still want the old combined behavior, this command will:

1. **Design** - Create ADR and Linear issues (via `/design` workflow)
2. **Implement** - Work on the feature (via `/implement` workflow)
3. **Ship** - Review, test, commit, push (via `/ship` workflow)

Feature to design and implement: $ARGUMENTS

## Recommendation

For better control and context management, run the commands separately:

```
/design <feature description>
# Review and approve the design
/implement <RUE-id>
# Work on implementation
/ship
```
