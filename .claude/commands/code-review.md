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
4. **Style** - Follows Rust idioms and project conventions?
5. **Error handling** - Appropriate error types and messages?
6. **Tests** - Are changes adequately tested?
7. **Documentation** - Are public APIs documented? Is the language specification updated if it needs to be? Are there spec tests added if so?

For the Rue compiler specifically, also check:
- Index-based references used correctly (no dangling indices)
- IR transformations preserve semantics
- Span tracking maintained for error reporting

Provide specific, actionable feedback with file:line references. File new bugs with `bd` for simple non-blocking improvemnets, but the feedback given here is things that we should land before we merge this change.
