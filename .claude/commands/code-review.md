---
description: Review code changes for quality and correctness
allowed-tools: Bash, Read, Glob, Grep
---

## Context

Current changes to review:

```
!jj show
```

## Your Task

Consider the way that a group of Rust experts and a group of compiler experts would review a change like this.

Review the current changes for:

1. **Correctness** - Does the code do what it's supposed to? Are there any bugs you can find?
2. **Performance** - Are we going to regress performance of the compiler itself or of the code the compiler generates? Is that an acceptable amount or not?
3. **Style** - Follows Rust idioms and project conventions?
4. **Error handling** - Appropriate error types and messages?
5. **Tests** - Are changes adequately tested? Consider:
   - **Spec tests** (`crates/rue-spec/cases/`): Required for language semantics changes. Must include `spec = [...]` references.
   - **UI tests** (`crates/rue-ui-tests/cases/`): Required for warnings, diagnostic changes, or compiler flag changes.
   - **Unit tests**: For internal implementation details.
6. **Specification** - If this changes language semantics:
   - Is the language specification (`docs/spec/src/`) updated with proper paragraph IDs?
   - Do new spec tests reference the new spec paragraphs?
   - Will traceability check pass (100% coverage required)?

For the Rue compiler specifically, also check:
- Index-based references used correctly (no dangling indices)
- IR transformations preserve semantics
- Span tracking maintained for error reporting
- **Multi-backend consistency**: If changes touch `rue-codegen`, verify that equivalent changes were made to ALL backends (x86_64 and aarch64). Check mir.rs, emit.rs, regalloc.rs, liveness.rs, and cfg_lower.rs in both directories.

Provide specific, actionable feedback with file:line references. File new bugs with `bd` for simple non-blocking improvements, but the feedback given here is things that we should land before we merge this change.
