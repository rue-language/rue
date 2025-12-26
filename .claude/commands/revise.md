---
description: Address PR feedback or CI failures, amend, and push
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task, WebFetch
---

## Task

Address feedback on a PR (CI failures or review comments), fix the issues, and push an update.

## Workflow

### 1. Identify the Issue

Check for:
- CI failure logs
- PR review comments
- User description of what needs fixing

If a PR URL is provided, fetch the details:
```bash
gh pr view <number> --comments
gh pr checks <number>
```

### 2. Understand the Failure

For CI failures:
- Which platform failed? (Linux x86-64, macOS ARM64, etc.)
- What test failed?
- What's the error message?

For review comments:
- What change is requested?
- Is it a blocking issue or suggestion?

### 3. Fix the Issues

Make the necessary code changes to address the feedback.

If a test failed on a different platform:
- Check if it's a platform-specific codegen issue
- Look at both `x86_64/` and `aarch64/` backends if relevant
- Ensure the fix works on all platforms

### 4. Run Tests Locally

```bash
./test.sh
```

### 5. Squash into the Previous Commit

```bash
jj squash
```

This amends the changes into the parent commit.

### 6. Push the Update

```bash
jj git push
```

This force-pushes the updated change.

## Output

```
## Revision Pushed

**Fixed:**
- CI failure on macOS: missing aarch64 instruction variant for Cmp64
- Added `Cmp64RR` to aarch64/mir.rs and aarch64/emit.rs

**Tests:** all passing locally
**Pushed:** force-updated feature/array-slices
```

## Common CI Failure Patterns

### Platform-specific codegen
If x86-64 passes but aarch64 fails (or vice versa):
- Check that new MIR instructions exist in both backends
- Check emission logic handles all variants
- Check register allocation covers new instructions

### Test flakiness
- Check for timing-dependent tests
- Check for order-dependent test state

### Spec traceability
- Ensure all spec paragraphs have covering tests
- Check for typos in `spec = ["X.Y:Z"]` references

## Important

- Use `jj squash` to amend, not `jj commit`
- Force push is expected - the branch is being revised
- If multiple rounds of feedback, repeat this process
- Don't close bd issues again - they were closed in `/ship`
