---
description: Plan and implement a new feature
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Context

You are working on the Rue programming language compiler. Review the project structure and CLAUDE.md for context.

## Your Task

Plan and implement this feature: $ARGUMENTS

Follow this workflow:

1. **Understand the request** - Clarify requirements if needed
2. **Check for existing issues** - Run `bd ready --json` to see if this is already tracked
3. **Create a bd issue** - Track this work with `bd create "<title>" -t feature -p 2 --json`
4. **Plan the implementation** - Identify which crates need changes (see Architecture in CLAUDE.md)
5. **Add spec tests** - Add test cases to `crates/rue-spec/cases/` as appropriate. Also update the language specification `docs/language.md` as appropriate.
6. **Implement incrementally** - Make changes, add tests as you go
7. **Verify** - Run `./test.sh` to ensure everything passes
8. **Add example** - consider adding or modifying programs in the `examples` directory that show off this feature.
9. **Close the issue** - `bd close <id> --reason "Implemented"`

Remember: This project uses Buck2 (`./buck2`), not Cargo. Use jj for version control.
