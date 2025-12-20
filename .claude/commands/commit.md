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

## Your Task

Create a commit for the current changes.

If a message was provided: $ARGUMENTS
- Use it as the basis for the commit message

If no message was provided:
- Analyze the changes and write an appropriate commit message

Commit message guidelines:
- Use imperative mood ("Add feature" not "Added feature")
- First line: concise summary (50 chars or less preferred)
- If needed, add blank line then detailed explanation
- Reference bd issue IDs if applicable (e.g., "Fixes bd-42")
- Consider what someone reading this in the future would want to see
  - Is the message too in-the-weeds? Things like a list of changed files are already found in the VCS tooling, and so are irrelevant here.
  - Does the message adequately describe the changes made?
  - Does the message provide context for why these changes were made?

If there are bd issues related to this work that should be closed, also run:
`bd close <id> --reason "Completed in commit"`

Use `jj commit -m "<message>"` to create the commit as the final step.

IMPORTANT: close the issue with `bd close` **before** you run `jj commit`, as we want to make sure that the issue being closed is associated with this commit.
