---
description: Create a commit with a descriptive message
allowed-tools: Bash(jj:*)
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
- Reference Linear issues: "Fixes RUE-42" or "Related to RUE-42"

## Workflow

1. **Create the commit**:
   ```bash
   jj commit -m "<message>"
   ```

2. **Mark related Linear issues Done** (after committing): use the Linear MCP tools (`save_issue` with state "Done")

## Important

- Commit first, then mark issues Done in Linear
- Each commit should leave tests passing
- Don't include file lists (VCS shows that)
- Don't commit WIP or debug code
