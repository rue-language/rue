---
description: Plan and implement a new feature (use /design + /implement instead)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Deprecated

This command is deprecated. Use the new workflow instead:

1. **`/design`** - Design the feature (creates ADR + bd issues)
2. **`/implement <bd-id>`** - Implement a bd issue
3. **`/ship`** - Rebase, review, test, commit, push

## Legacy Behavior

If you still want the old combined behavior, this command will:

1. **Design** - Create ADR and bd issues (via `/design` workflow)
2. **Implement** - Work on the feature (via `/implement` workflow)
3. **Ship** - Review, test, commit, push (via `/ship` workflow)

Feature to design and implement: $ARGUMENTS

## Recommendation

For better control and context management, run the commands separately:

```
/design <feature description>
# Review and approve the design
/implement <bd-id>
# Work on implementation
/ship
```
