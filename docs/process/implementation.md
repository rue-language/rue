# Implementing Features

This document describes how to implement planned features. The `/implement` command automates this workflow.

## Prerequisites

Before implementing:
1. A plan exists (bd issue, and ADR for large features)
2. You understand what needs to be done
3. The work fits in a single session (split if not)

## Step 1: Load the Context

1. **Get the issue details**:
   ```bash
   bd show <bd-id> --json
   ```

2. **For large features**, read the ADR:
   - Find it in `docs/designs/`
   - Identify which phase you're implementing
   - Understand how this phase fits into the whole

3. **Mark work in progress**:
   ```bash
   bd update <bd-id> --status in_progress --json
   ```

## Step 2: Scope Check

Verify the work fits in the current session:

**Good scope (proceed):**
- Clear, bounded changes
- 1-5 files to modify
- Single logical unit of work

**Too large (split it):**
- More than 5-7 files
- Multiple unrelated changes
- Would require extensive exploration

If too large, create subtasks:
```bash
bd create "Subtask: <description>" --parent <bd-id> -t task --json
```

Then implement subtasks one at a time.

## Step 3: Implementation Order

Follow this order for consistent, reviewable changes:

### 1. Update Specification (if changing language semantics)

Edit files in `docs/spec/src/`:
- Add or modify paragraphs with proper IDs: `r[X.Y:Z#category]`
- Categories: `normative`, `legality-rule`, `dynamic-semantics`, `syntax`, `example`, `informative`

### 2. Add Tests First

**Spec tests** (`crates/rue-spec/cases/`):
- For language semantics
- Must include `spec = ["X.Y:Z"]` references to spec paragraphs
- Traceability check enforces 100% coverage

**UI tests** (`crates/rue-ui-tests/cases/`):
- For warnings, diagnostics, compiler flags
- Not tied to spec paragraphs

**Unit tests** (in crate source with `#[cfg(test)]`):
- For internal implementation details
- Data structure methods, algorithms, edge cases

### 3. Make Code Changes

Follow existing patterns. Key considerations:

**Multi-backend consistency**: If touching `rue-codegen`, implement in ALL backends:
- `x86_64/` - Linux x86-64
- `aarch64/` - macOS ARM64

Check: `mir.rs`, `emit.rs`, `regalloc.rs`, `liveness.rs`, `cfg_lower.rs`

**Index-based references**: Use u32 indices, not pointers. Check for dangling indices.

**Span tracking**: Maintain source locations for error reporting.

### 4. For Preview Features

If this is a large feature behind a preview gate:

**Add tests with preview flag**:
```toml
[[case]]
name = "my_feature_test"
preview = "my_feature"
source = """..."""
exit_code = 42
```

**Add semantic gates**:
```rust
if using_preview_syntax {
    self.require_preview(PreviewFeature::MyFeature, "feature description", span)?;
}
```

Preview tests run but are allowed to fail until the feature is complete.

## Step 4: Verify

Run the full test suite:
```bash
./test.sh
```

This runs:
- Unit tests (`./buck2 test //...`)
- Spec tests (`./buck2 run //crates/rue-spec:rue-spec`)
- Traceability check

**For stable work**: All tests must pass.

**For preview features**: Stable tests must pass. Preview tests for your feature should pass when your phase is complete.

## Step 5: Update Progress

For large features, update the ADR checkbox:

```markdown
## Implementation Phases

- [x] **Phase 1: Core parsing** - bd-42
- [ ] **Phase 2: Type checking** - bd-43
```

## Step 6: Review and Commit

1. Run code review: `/code-review` (see [code-review.md](code-review.md))
2. Fix any blocking issues
3. Commit: `/commit` (see [committing.md](committing.md))

## Stabilizing a Large Feature

When all phases are complete:

1. **Remove preview gates** from spec tests (delete `preview = "..."`)
2. **Remove `require_preview()` calls** from semantic analysis
3. **Remove the feature** from `PreviewFeature` enum
4. **Update ADR status** to "Implemented"
5. **Fill in frontmatter dates** (`implemented:`)
6. **Create stabilization commit**

## Common Patterns

### Adding a New Operator

1. Lexer: Add token in `rue-lexer`
2. Parser: Add parsing in `rue-parser`
3. RIR: Add IR node in `rue-rir`
4. AIR: Add typed node in `rue-air`
5. Sema: Add type checking in `rue-air/src/sema.rs`
6. Codegen: Add code generation in both backends

### Adding a New Type

1. Add to `Type` enum in `rue-air/src/types.rs`
2. Update type checking in sema
3. Update code generation for the type's operations
4. Add spec chapter and tests

### Adding a New Statement/Expression

1. Parser: Add syntax handling
2. RIR/AIR: Add IR representation
3. Sema: Add semantic analysis
4. Codegen: Add code generation
5. Spec: Document the syntax and semantics
