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

**Do NOT write an ADR for small features** like adding a single operator, fixing bugs, or simple refactoring. Those just need a Linear issue (team "Rue").

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
- [ ] **Phase 1: Core parsing** - RUE-NNN
- [ ] **Phase 2: Type checking** - RUE-NNN
```

**Consequences**: Positive, negative, and neutral implications

**Open Questions**: Unresolved issues (for proposals)

**Future Work**: Out of scope for this ADR, but related

### 4. Create Linear Issues

After the ADR is drafted, file the tracking issues in Linear (team "Rue") via the
Linear MCP tools — see the "Issue Tracking with Linear" section of the root
`CLAUDE.md` for the full workflow:

- Create a parent (epic) issue with `save_issue` (`team: "Rue"`), linking the ADR in the description
- Create a sub-issue per phase with `save_issue` (`parentId: <epic RUE-NN>`)

Update the ADR with the resulting Linear issue IDs (`RUE-NN`).

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
| [0006](0006-zola-unified-website.md) | Unified Zola Website | Implemented | tooling, documentation |
| [0007](0007-hindley-milner-inference.md) | Hindley-Milner Type Inference | Implemented | types, compiler |
| [0008](0008-affine-types-mvs.md) | Affine Types and Mutable Value Semantics | Accepted | types, semantics, ownership |
| [0009](0009-struct-methods.md) | Struct Methods | Proposal | types, syntax |
| [0044](0044-optimization-levels.md) | Optimization Levels (-O0/-O1/-O2/-O3) | Accepted | compiler, codegen, process |
| [0045](0045-lazy-semantic-analysis.md) | Lazy Semantic Analysis (compile-on-reference) | Accepted | compiler, semantics, comptime |
