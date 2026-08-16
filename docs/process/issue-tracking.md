# Issue Tracking with Linear

This document describes how we track work using [Linear](https://linear.app), in the **Rue** team. Issues are identified as `RUE-NN`.

## Why Linear?

- **Dependency-aware**: Track blockers and relationships between issues
- **Hierarchical**: Parent issues (epics) with sub-issues
- **Agent-accessible**: Claude Code uses the Linear MCP tools
- **Shared**: One source of truth for humans and agents

## Access

There is no CLI. From Claude Code, use the Linear MCP tools:

| Tool | Use For |
|------|---------|
| `list_issues` | Find work (filter by team, state, assignee) |
| `get_issue` | View an issue's details |
| `save_issue` | Create or update an issue (state, assignee, parent, labels) |
| `save_comment` | Add notes or findings to an issue |

## Issue Labels

Labels mirror the issue types we use:

| Label | Use For |
|-------|---------|
| `bug` | Something broken |
| `feature` | New functionality |
| `task` | Work item (tests, docs, refactoring) |
| `chore` | Maintenance (dependencies, tooling) |

Epics don't need a label - they're just issues with sub-issues.

## Priorities

Linear priority semantics:

| Priority | Meaning |
|----------|---------|
| 1 (Urgent) | Critical (security, data loss, broken builds) |
| 2 (High) | Major features, important bugs |
| 3 (Medium) | Default, standard work |
| 4 (Low) | Polish, optimization, backlog ideas |

## Issue Lifecycle

```
Todo/Backlog → In Progress → Done
```

### Creating Issues

Use `save_issue` with `team: "Rue"`, a clear title, and a Markdown description.

- **Standalone work**: e.g. "Add modulo operator" with the `feature` label, priority 3
- **Work discovered during other work**: create the issue with `relatedTo` pointing at the issue you were working on (or `blockedBy` if it's a true dependency)
- **Subtasks of an epic**: create with `parentId` set to the epic

### Working on Issues

1. **Find work**: `list_issues` with state `Todo` or `Backlog`; skip issues blocked by open issues
2. **Claim it**: `save_issue` with state `In Progress` and assignee `me`
3. **Do the work**: Implement, test, review
4. **Complete it**: commit, then `save_issue` with state `Done`

### Issue States

- **Todo / Backlog**: Not started
- **In Progress**: Being worked on
- **Done**: Completed

## Epics and Sub-Issues

Large features use a parent issue (the "epic") with sub-issues:

1. Create the epic: `save_issue` with title "Implement enums" (returns e.g. RUE-10)
2. Create sub-issues with `parentId` set to the epic:
   - "Phase 1: Lexer and parser"
   - "Phase 2: Type system"
   - "Phase 3: Code generation"

Sub-issues can be worked independently. Mark the epic Done when all sub-issues are done.

## Relationships

Use issue relations to express dependencies:

- **`blockedBy`**: this issue depends on another being completed first (e.g. "Optimize enum matching" blocked by the enum implementation issue)
- **`relatedTo`**: this was discovered while working on another issue (e.g. "Edge case in parser" related to the issue you were working on)

Skip blocked issues when looking for ready work.

## Linking to ADRs

For large features, the Linear epic and ADR reference each other:

**In the ADR** (`docs/designs/<NNNN>-feature.md`):
```markdown
## Implementation Phases

- [ ] **Phase 1: Parsing** - RUE-42
- [ ] **Phase 2: Types** - RUE-43
```

**In the Linear issue description**: Reference the ADR file path.

## Best Practices

1. **Reference issue IDs in commit messages** (`Fixes RUE-42`) - Linear isn't stored in the repo, so the commit message is the link
2. **Commit first, then mark the issue Done** - the issue state lives in Linear, not in the commit
3. **Link discovered work** with `relatedTo` (or `blockedBy`) relations
4. **Check `Todo`/`Backlog` issues** before asking "what should I work on?"
5. **Use epics** (parent issues + sub-issues) for multi-phase features
6. **Keep descriptions focused** - details go in ADRs, not issue descriptions

## Common Workflows

### Starting a Session

1. `list_issues` in the Rue team with state `Todo` or `Backlog` - what can I work on?
2. `get_issue` - details of a specific issue
3. `save_issue` with state `In Progress`, assignee `me`

### Finishing Work

1. `jj commit -m "... Fixes RUE-NN"`
2. `save_issue` with state `Done`

### Found a Bug While Working

Create a new issue ("Found: null pointer in edge case", `bug` label, priority 2) with `relatedTo` pointing at the current issue. Continue with the original work, or switch to the bug if it's blocking.

### Splitting Work That's Too Big

Create sub-issues ("Part 1: <description>", "Part 2: <description>") with `parentId` set to the original issue. The original issue becomes the parent epic.
