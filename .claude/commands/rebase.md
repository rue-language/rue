---
description: Rebase on trunk and auto-fix conflicts
allowed-tools: Bash(jj:*), Read, Write, Edit, Glob, Grep
---

## Task

Rebase the current change on trunk and automatically resolve any conflicts.

## Workflow

### 1. Rebase on trunk

```bash
jj rebase -d trunk
```

### 2. Check for conflicts

```bash
jj status
```

If there are conflicts, resolve them automatically.

### 3. Resolve Conflicts

For each conflicted file:

1. **View the conflict** with `--git` format for clarity:
   ```bash
   jj show --git
   ```

2. **Understand context** - Read commit messages to understand what each side intended:
   ```bash
   jj log -r 'trunk..@'
   ```

3. **Resolve intelligently**:
   - Default assumption: usually want both changes merged
   - Use commit messages to understand intent
   - If changes are to different parts of a file, include both
   - If truly conflicting, pick what makes sense based on context
   - Do NOT ask - just fix it

4. **Edit the file** to remove conflict markers and apply the resolution

5. **Mark resolved**:
   ```bash
   jj resolve --mark <file>
   ```

### 4. Verify

```bash
jj status
jj diff --git
```

## Conflict Resolution Strategy

- **Both changes wanted** (most common): Merge both modifications
- **Semantic conflict**: Use commit context to pick the right approach
- **Deletion vs modification**: Usually keep the modification unless the deletion was intentional cleanup
- **Same line, different changes**: Understand what each was trying to do, synthesize

## Output

Report what was done:
```
Rebased on trunk.
- Resolved 2 conflicts in src/foo.rs (merged both changes)
- Resolved 1 conflict in src/bar.rs (kept trunk's deletion of deprecated fn)
```

Or if clean:
```
Rebased on trunk. No conflicts.
```

## Important

- Always use `--git` flag for diffs to avoid jj's default format confusion
- Never ask about conflicts - resolve them automatically
- Use commit messages to understand intent when resolving
