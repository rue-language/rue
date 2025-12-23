---
description: Create a commit with a descriptive message
allowed-tools: Bash(jj:*), Bash(bd:*)
argument-hint: [commit message]
---

## Context

Current change:
```
!jj show
```

## Task

Create a commit following `docs/process/committing.md`.

If a message was provided: $ARGUMENTS
- Use it as the basis for the commit message

If no message was provided:
- Analyze the changes and write an appropriate message

## Commit Message Guidelines

- Use imperative mood ("Add feature" not "Added feature")
- First line: concise summary (50 chars preferred, 72 max)
- Optional body: explain what and why (not how)
- Reference bd issues: "Fixes bd-42" or "Related to bd-42"

## Workflow

1. **Close related bd issues first** (so closure is in the commit):
   ```bash
   bd close <id> --reason "Completed"
   ```

2. **Create the commit**:
   ```bash
   jj commit -m "<message>"
   ```

## Important

- Close issues with `bd close` BEFORE `jj commit`
- Each commit should leave tests passing
- Don't include file lists (VCS shows that)
- Don't commit WIP or debug code
