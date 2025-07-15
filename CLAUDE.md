# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. For human developer documentation, see [CONTRIBUTING.md](./CONTRIBUTING.md).

## Project Overview

Rue is a programming language compiler. You should refer to the following resources:
- [docs/spec.md](./docs/spec.md) - Complete language specification
- [docs/implementation.md](./docs/implementation.md) - Implementation details
- [CONTRIBUTING.md](./CONTRIBUTING.md) - Development commands and workflows

## Claude-Specific Instructions

### Code References
When referencing specific functions or pieces of code, include the pattern `file_path:line_number` to allow easy navigation to the source code location.

Example: "Clients are marked as failed in the `connectToServer` function in src/services/process.ts:712."

### When Working on the Rue Compiler

1. **Always check existing patterns** - Before implementing new features, examine how similar features are implemented
2. **Follow the specification** - The language spec in docs/spec.md is authoritative
3. **Test thoroughly** - Run the appropriate test suite after making changes
4. **Update documentation** - Keep spec.md, implementation.md, and session documentation current

### Important Constraints

- **Platform**: Linux x86-64 only (generates ELF executables)
- **Variables**: All variables are 64-bit integers
- **Build System**: Buck2 primary, Cargo as fallback
- **Version Control**: jj (Jujutsu) exclusively, never use git commands

### Documentation Workflow

When working on features:
1. Check for existing `implementation-plan.md` in session directories
2. Follow the Implementation Checklist with small, numbered tasks
3. Update task checkboxes as work progresses
4. Commit frequently with concise messages

### Testing Commands Quick Reference

- `buck2 test //crates/rue-lexer:test` - Lexer tests
- `buck2 test //crates/rue-parser:test` - Parser tests  
- `buck2 test //crates/rue-semantic:test` - Semantic tests
- `buck2 test //crates/rue-codegen:test` - Codegen tests
- `buck2 test //crates/rue-compiler:test` - Compiler tests
- `buck2 test //crates/rue:integration_tests` - Integration tests
- `buck2 test //crates/rue:type_system_tests` - Type system tests
- `buck2 test //crates/rue:` - All rue tests (integration + type system)
- `buck2 test //crates/...` - **All tests (equivalent to `cargo test`)**
- `cargo test -p rue` - Integration tests
- `cargo test` - All tests

## Version Control

**IMPORTANT**: This project uses jj (Jujutsu) exclusively. NEVER use git commands.

### Basic jj Commands
- `jj status` - Show current changes and working copy status
- `jj commit -m "message"` - Commit current changes with a message
- `jj describe -m "message"` - Set/update description of current change
- `jj log` - View commit history (graphical view)
- `jj diff` - Show diff of current changes
- `jj files` - List files changed in working copy
- `jj show` - Show details of current commit

### Branch Management
- `jj new` - Create new change (equivalent to git checkout -b)
- `jj new trunk` - Create new change based on trunk
- `jj edit <commit>` - Switch to editing a specific commit
- `jj abandon` - Abandon current change

### Synchronization and Pull Requests
- `jj git fetch` - Fetch changes from remote repository
- `jj rebase -d trunk` - Rebase current change onto trunk
- `jj git push -c @` - Push current change and create bookmark (@ refers to current change)

**Pull Request Workflow:**
1. `jj new trunk` - Create new change from trunk
2. Make your changes and test them
3. `jj commit -m "descriptive message"` - Commit changes
4. `jj git push -c @` - Push to remote and create bookmark (note the bookmark name from output)
5. `gh pr create --head <bookmark-name-from-step-4>` - Create pull request using the bookmark name shown in previous step
6. After review: `jj git fetch && jj rebase -d trunk` if needed

**Pull Request Message Guidelines:**
- Keep descriptions concise and technical, avoid LLM-style verbosity
- Focus on what was changed, not implementation details
- Use bullet points for multiple changes
- Avoid phrases like "This PR", "I have implemented", or overly formal language
- Example: "Add while loops to parser and codegen" rather than "This pull request implements comprehensive while loop support across the compiler pipeline"

### Commit Message Guidelines
- Write in imperative mood and present tense
- Be descriptive about what the change accomplishes
- **If the change modifies spec.md**: Use the language specification change as the primary basis for the commit message, as this describes the fundamental change being made to the language
- Examples: "Add while loop support to parser", "Fix segfault in code generation", "Add assignment operators to the language"

### Advanced Operations
- `jj split` - Split current change into multiple commits
- `jj squash` - Squash changes into parent commit
- `jj duplicate` - Create duplicate of current change
- `jj restore <file>` - Restore file from parent commit

## Documentation Maintenance

**CRITICAL**: Keep project documentation up to date with implementation changes:

### Language Specification Workflow
When implementing new language features or modifying existing language behavior:

1. **Update the formal specification FIRST** - Modify [docs/spec.md](./docs/spec.md) before implementing
2. **Specification-driven development** - The language spec is the authoritative definition of Rue
3. **Update examples and tests** - Ensure spec examples match implementation behavior
4. **Version the specification** - Track language version changes in the spec document
5. **Validate against spec** - Implementation must conform to the formal specification

The formal language specification in `docs/spec.md` is designed to be implementation-independent. Any alternative Rue compiler should be able to use this specification to achieve compatibility.

**Workflow for language changes:**
1. Propose specification changes in `docs/spec.md`
2. Update grammar, semantics, and examples as needed
3. Implement the feature in the compiler
4. Verify implementation matches specification exactly
5. Add conformance tests that validate spec compliance

### README Maintenance
When making significant changes to the project:

1. **Update README.md** - Keep the README accurate with current features and capabilities
2. **Verify technical accuracy** - Test all code examples and commands in the README
3. **Update build instructions** - Ensure both Cargo and Buck2 examples work correctly
4. **Language feature updates** - When adding/removing language features, update the feature list
5. **Sample programs** - Verify example programs still compile and run as described

### Design Documentation Workflow

When working on significant features or architectural changes:

**Documentation Structure:**
- `docs/sessions/session-N-feature-name/` - Contains all session-related documentation
  - `session-summary.md` - **Permanent** - High-level summary of what was accomplished
  - `design-decisions.md` - **Permanent** - Design rationale, alternatives considered, trade-offs
  - `implementation-plan.md` - **Temporary** - Step-by-step implementation tasks, deleted when complete
- `docs/architecture/feature-name/` - **Permanent** - Technical specifications and guides
- `docs/decisions/NNN-feature-name.md` - **Permanent** - Numbered Architecture Decision Records (ADRs)

**Workflow for Major Features:**
1. **Start new session** - Create `docs/sessions/session-N-feature-name/` directory
2. **Document design decisions** - Record why specific choices were made, alternatives considered
3. **Create implementation plan** - Break down work into concrete tasks with checkboxes (temporary document)
4. **Implement feature** - Follow the implementation plan, check off completed tasks
5. **Create permanent documentation** - Move key specs to `docs/architecture/`
6. **Write session summary** - Document what was accomplished and lessons learned
7. **Clean up** - Delete temporary implementation plans, keep design decisions and summaries

**Returning to Work:**
- If `implementation-plan.md` exists in a session directory, that's the current active work
- Read `design-decisions.md` first for context when returning to implementation
- Check the **Implementation Checklist** section for small, actionable tasks with checkboxes
- Update checkboxes as work progresses to track completion status
- Check `docs/architecture/` for formal specifications and technical details

**Implementation Plan Format:**
- Include an **Implementation Checklist** at the top with numbered, actionable tasks
- Use checkboxes `- [ ]` for easy progress tracking
- Break large tasks into small steps (15-30 minutes each)
- Number tasks (e.g., **1.1a**, **1.1b**) for easy reference
- Keep checklist updated as work progresses
- **Commit each step** with concise messages for checkpoint tracking (can be squashed later)
- **Update checklist before committing** - mark tasks complete before making the commit

**Important:** Implementation plans are working documents that get deleted when complete. Design decisions and session summaries are permanent historical records.