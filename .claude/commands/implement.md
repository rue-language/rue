---
description: Implement a planned feature from a bd issue
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <bd-id>
---

## Task

Implement the feature tracked by: $ARGUMENTS

## Instructions

Read and follow `docs/process/implementation.md` for the full implementation workflow.

Key references:
- `docs/process/implementation.md` - Implementation workflow
- `docs/process/code-review.md` - Review standards
- `docs/designs/` - ADRs for large features

## Summary

1. **Load context** - `bd show <id>`, read ADR if applicable, mark in_progress
2. **Scope check** - Ensure work fits in one session (split if not)
3. **Implement** in order:
   - Update specification (`docs/spec/src/`) if changing language semantics
   - Add tests first (spec tests, UI tests, or unit tests as appropriate)
   - Make code changes (check ALL backends if touching codegen)
4. **Verify** - Run `./test.sh`
5. **Update progress** - Check off phase in ADR if applicable
6. **Review and commit** - `/code-review` then `/commit`

## For Preview Features

- Add tests with `preview = "<feature>"` flag
- Add `require_preview()` gates in semantic analysis
- Stable tests must always pass; preview tests should pass when phase is complete

## Stabilization (when all phases complete)

1. Remove `preview = "..."` from tests
2. Remove `require_preview()` calls
3. Remove feature from `PreviewFeature` enum
4. Update ADR status to "Implemented"
5. Fill in `implemented:` date in ADR frontmatter

## Important

- If touching `rue-codegen`, implement in ALL backends (x86_64 and aarch64)
- Each commit should leave tests passing
- Split work that's too large into subtasks
- Use Buck2 (`./buck2`), not Cargo
- Use `jj` for version control, not git
