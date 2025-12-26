---
description: Auto-resolve merge conflicts
allowed-tools: Bash(jj:*), Read, Write, Edit, Glob, Grep
---

## Task

Detect and automatically resolve any merge conflicts in the current change.

## Workflow

### 1. Identify Conflicts

```bash
jj status
```

Look for files marked as conflicted.

### 2. For Each Conflicted File

1. **View the conflict** with `--git` format:
   ```bash
   jj diff --git <file>
   ```

   Or view the full file:
   ```bash
   jj show --git
   ```

2. **Understand context** - Check what each side was trying to do:
   ```bash
   jj log -r 'trunk..@' --no-graph
   ```

3. **Read the file** and understand the conflict markers

4. **Resolve** by editing the file to remove markers and apply the correct resolution

5. **Mark resolved**:
   ```bash
   jj resolve --mark <file>
   ```

### 3. Verify Resolution

```bash
jj status
jj diff --git
```

## Conflict Resolution Strategy

### Default: Merge Both Changes
Most conflicts happen because both sides made independent changes. Include both.

### Semantic Conflicts
When changes actually conflict in meaning:
- Read commit messages to understand intent
- Consider which change is "newer" conceptually
- Pick the approach that makes sense for the codebase

### Deletion vs Modification
- If trunk deleted something your branch modified: usually keep the modification
- If trunk modified something your branch deleted: check if the deletion was intentional cleanup

### Same Line, Different Values
- Understand what each side was trying to achieve
- Synthesize a solution that accomplishes both goals
- If impossible, pick based on context

## Output

```
Resolved 3 conflicts:
- src/foo.rs: merged both changes (independent modifications)
- src/bar.rs: kept branch's new function (trunk just reformatted)
- src/baz.rs: used trunk's constant value (branch had stale data)
```

## Important

- Always use `--git` flag for diffs
- Never ask about conflicts - resolve them automatically
- Use commit messages and code context to make smart decisions
- If unsure, prefer keeping more code over less (easier to delete than recreate)
