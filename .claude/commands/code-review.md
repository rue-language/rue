---
description: Review code changes for quality and correctness
allowed-tools: Bash, Read, Glob, Grep
---

## Context

Current changes to review:

```
!jj show
```

## Task

Review the current changes following `docs/process/code-review.md`.

## Review Checklist

1. **Correctness** - Does the code do what it's supposed to? Any bugs?
2. **Performance** - Compiler performance and generated code quality
3. **Style** - Rust idioms, project conventions
4. **Error handling** - Appropriate types and clear messages
5. **Tests** - Adequate coverage:
   - Spec tests (`crates/rue-spec/cases/`) with `spec = [...]` for language semantics
   - UI tests (`crates/rue-ui-tests/cases/`) for warnings/diagnostics
   - Unit tests for internal implementation details
6. **Specification** - If changing language semantics:
   - Is `docs/spec/src/` updated with proper paragraph IDs?
   - Will traceability check pass (100% coverage)?

## Rue-Specific Checks

- **Index-based references** - No dangling indices
- **IR transformations** - Semantics preserved
- **Span tracking** - Source locations maintained for errors
- **Multi-backend** - If touching `rue-codegen`, check BOTH x86_64 and aarch64:
  - `mir.rs`, `emit.rs`, `regalloc.rs`, `liveness.rs`, `cfg_lower.rs`

## Output

Provide specific, actionable feedback with file:line references.

**Blocking issues**: Must fix before commit
**Non-blocking**: File as bd issues for later

Consider the way that a group of Rust experts and compiler experts would review this change.
