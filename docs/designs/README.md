# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records for the Rue project. ADRs document significant design decisions, providing historical context for why things are the way they are.

## What is an ADR?

An ADR captures:
- **Context**: Why a decision was needed
- **Decision**: What we chose to do
- **Consequences**: Trade-offs and implications

ADRs are the historical record of "why did we do it this way?"

## When to Write an ADR

Write an ADR for **large features** that:
- Touch many files across multiple crates
- Have multiple implementation phases
- Add new runtime functions or IR instructions
- Introduce new type system concepts
- May span multiple development sessions

**Do NOT write an ADR for small features** like adding a single operator, fixing bugs, or simple refactoring. Those just need a bd issue.

**Rule of thumb**: If it needs a preview feature gate, it needs an ADR.

## ADR Lifecycle

```
proposal → accepted → implemented
```

| Status | Meaning |
|--------|---------|
| `proposal` | Under discussion, not yet approved |
| `accepted` | Design approved, implementation in progress |
| `implemented` | Feature complete, language parts in spec |
| `superseded` | Replaced by a newer ADR |

When a feature is implemented:
1. Language semantics move to the [specification](../spec/)
2. The ADR becomes historical reference
3. Status updates to `implemented`

## Creating an ADR

### 1. Copy the Template

```bash
cp docs/designs/0000-template.md docs/designs/NNNN-<feature>.md
```

Use the next available 4-digit number.

### 2. Fill in the Frontmatter

```yaml
---
id: 0006
title: Your Feature Title
status: proposal
tags: [types, syntax]  # relevant tags
feature-flag: your-feature  # for preview gating
created: 2025-01-15
accepted:       # fill when accepted
implemented:    # fill when implemented
spec-sections: []  # fill when language parts move to spec
superseded-by:  # fill if superseded
---
```

### 3. Write the Content

**Summary**: One paragraph overview

**Context**: Why is this needed? What problem does it solve?

**Decision**: Technical details - syntax, semantics, implementation approach

**Implementation Phases**: Break into independently-committable chunks
```markdown
- [ ] **Phase 1: Core parsing** - bd-XXX
- [ ] **Phase 2: Type checking** - bd-XXX
```

**Consequences**: Positive, negative, and neutral implications

**Open Questions**: Unresolved issues (for proposals)

**Future Work**: Out of scope for this ADR, but related

### 4. Create bd Issues

After the ADR is drafted:

```bash
# Create epic
bd create "<feature title>" -t epic -p 2 --json

# Create subtask for each phase
bd create "Phase 1: <description>" -t task --parent <epic-id> --json
```

Update the ADR with the bd issue IDs.

### 5. Add Preview Feature

In `crates/rue-error/src/lib.rs`:
```rust
pub enum PreviewFeature {
    // ...
    YourFeature,
}
```

## ADR Structure

See [0000-template.md](0000-template.md) for the full template.

Required sections:
- Frontmatter (YAML)
- Status
- Summary
- Context
- Decision
- Implementation Phases
- Consequences

Optional sections:
- Open Questions (for proposals)
- Future Work
- References

## Tags

Use tags to categorize ADRs:

| Tag | For |
|-----|-----|
| `types` | Type system changes |
| `syntax` | Language syntax |
| `semantics` | Runtime behavior |
| `compiler` | Compiler internals |
| `process` | Development process |

Tags are freeform - add new ones as needed.

## Relationship to Preview Features

ADRs and preview features are tightly coupled:

- Every ADR has a `feature-flag` in its frontmatter
- The flag gates the feature during development
- When the feature is complete, the gate is removed
- The ADR status changes to `implemented`

See [ADR-0005: Preview Features](0005-preview-features.md) for details on the gating mechanism.

## Index

| ID | Title | Status | Tags |
|----|-------|--------|------|
| [0000](0000-template.md) | Template | - | - |
| [0001](0001-never-type.md) | The Never Type | Implemented | types |
| [0002](0002-single-pass-bidirectional-types.md) | Single-Pass Bidirectional Type Checking | Implemented | compiler |
| [0003](0003-constant-evaluation.md) | Constant Expression Evaluation | Implemented | compiler |
| [0004](0004-enum-types.md) | Enum Types | Implemented | types, syntax |
| [0005](0005-preview-features.md) | Preview Features | Accepted | process |
