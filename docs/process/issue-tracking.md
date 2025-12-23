# Issue Tracking with bd (beads)

This document describes how we track work using [bd (beads)](https://github.com/steveyegge/beads).

## Why bd?

- **Dependency-aware**: Track blockers and relationships between issues
- **VCS-friendly**: Auto-syncs to `.beads/issues.jsonl` for version control
- **Agent-optimized**: JSON output, ready work detection
- **Local-first**: No external service required

## Quick Reference

```bash
# Find ready work
bd ready --json

# Create issues
bd create "Title" -t feature -p 2 --json
bd create "Subtask" --parent <epic-id> --json

# Update status
bd update <id> --status in_progress --json

# Complete work
bd close <id> --reason "Completed" --json

# View issue
bd show <id> --json
```

## Issue Types

| Type | Use For |
|------|---------|
| `bug` | Something broken |
| `feature` | New functionality |
| `task` | Work item (tests, docs, refactoring) |
| `epic` | Large feature with subtasks |
| `chore` | Maintenance (dependencies, tooling) |

## Priorities

| Priority | Meaning |
|----------|---------|
| 0 | Critical (security, data loss, broken builds) |
| 1 | High (major features, important bugs) |
| 2 | Medium (default, standard work) |
| 3 | Low (polish, optimization) |
| 4 | Backlog (future ideas) |

## Issue Lifecycle

```
open → in_progress → closed
```

### Creating Issues

For standalone work:
```bash
bd create "Add modulo operator" -t feature -p 2 --json
```

For work discovered during other work:
```bash
bd create "Found edge case bug" -t bug -p 1 --deps discovered-from:<parent-id> --json
```

For subtasks of an epic:
```bash
bd create "Phase 1: Parsing" -t task --parent <epic-id> --json
```

### Working on Issues

1. **Find work**: `bd ready` shows unblocked issues
2. **Claim it**: `bd update <id> --status in_progress`
3. **Do the work**: Implement, test, review
4. **Complete it**: `bd close <id> --reason "Completed"`

### Issue States

- **open**: Not started
- **in_progress**: Being worked on
- **closed**: Completed (with reason)

## Epics and Subtasks

Large features use epics with subtasks:

```bash
# Create epic
bd create "Implement enums" -t epic -p 2 --json
# Returns: bd-10

# Create subtasks
bd create "Phase 1: Lexer and parser" -t task --parent bd-10 --json
bd create "Phase 2: Type system" -t task --parent bd-10 --json
bd create "Phase 3: Code generation" -t task --parent bd-10 --json
```

Subtasks can be worked independently. Close the epic when all subtasks are done.

## Dependencies

Use `--deps` to express relationships:

```bash
# This issue depends on bd-5 being completed first
bd create "Optimize enum matching" -t task --deps bd-5 --json

# This was discovered while working on bd-10
bd create "Edge case in parser" -t bug --deps discovered-from:bd-10 --json
```

Issues with unmet dependencies won't appear in `bd ready`.

## Linking to ADRs

For large features, the bd epic and ADR reference each other:

**In the ADR** (`docs/designs/NNNN-feature.md`):
```markdown
## Implementation Phases

- [ ] **Phase 1: Parsing** - bd-42
- [ ] **Phase 2: Types** - bd-43
```

**In the bd issue description**: Reference the ADR file path.

## Auto-Sync

bd automatically syncs with version control:
- Exports to `.beads/issues.jsonl` after changes (5s debounce)
- Imports from JSONL when newer (e.g., after pulling)
- No manual export/import needed

**Important**: Commit `.beads/issues.jsonl` with your code changes so issue state stays in sync.

## Best Practices

1. **Always use `--json`** for programmatic/scripted use
2. **Close issues before committing** so the closure is in the same commit
3. **Link discovered work** with `discovered-from` dependencies
4. **Check `bd ready`** before asking "what should I work on?"
5. **Use epics** for multi-phase features
6. **Keep descriptions focused** - details go in ADRs, not issue descriptions

## Common Workflows

### Starting a Session

```bash
bd ready --json           # What can I work on?
bd show <id> --json       # Details of a specific issue
bd update <id> --status in_progress --json
```

### Finishing Work

```bash
bd close <id> --reason "Completed" --json
jj commit -m "..."
```

### Found a Bug While Working

```bash
bd create "Found: null pointer in edge case" -t bug -p 1 --deps discovered-from:<current-id> --json
# Continue with original work, or switch to the bug if it's blocking
```

### Splitting Work That's Too Big

```bash
bd create "Part 1: <description>" --parent <original-id> -t task --json
bd create "Part 2: <description>" --parent <original-id> -t task --json
# Original issue becomes the parent epic
```
