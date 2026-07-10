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
| `rejected` | Considered but deliberately not adopted, or rendered moot |

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

The table is generated from ADR frontmatter. Run
`scripts/validate-adrs.py --write` after adding or changing a record.

<!-- ADR-INDEX:START -->
| ID | Title | Status | Tags |
| --- | --- | --- | --- |
| [0000](0000-template.md) | ADR Title | Proposal | — |
| [0001](0001-never-type.md) | The Never Type | Implemented | types |
| [0002](0002-single-pass-bidirectional-types.md) | Single-Pass Bidirectional Type Checking | Superseded | compiler |
| [0003](0003-constant-evaluation.md) | Constant Expression Evaluation | Implemented | compiler |
| [0004](0004-enum-types.md) | Enum Types | Implemented | types, syntax |
| [0005](0005-preview-features.md) | Preview Features | Implemented | process |
| [0006](0006-zola-unified-website.md) | Unified Zola Website | Implemented | tooling, documentation |
| [0007](0007-hindley-milner-inference.md) | Hindley-Milner Type Inference | Implemented | types, compiler |
| [0008](0008-affine-types-mvs.md) | Affine Types and Mutable Value Semantics | Implemented | types, semantics, ownership |
| [0009](0009-struct-methods.md) | Struct Methods | Implemented | types, syntax |
| [0010](0010-destructors.md) | Destructors | Implemented | types, semantics, ownership, memory |
| [0011](0011-runtime-heap.md) | Runtime Heap | Implemented | runtime, memory, allocator |
| [0012](0012-optimization-passes.md) | Compiler Optimization Passes | Implemented | compiler, codegen |
| [0013](0013-borrowing-modes.md) | Borrowing Modes | Implemented | types, semantics, ownership, borrowing |
| [0014](0014-mutable-strings.md) | Mutable Strings | Implemented | types, memory, strings |
| [0015](0015-test-suite-optimization.md) | Test Suite Optimization | Implemented | process, testing, compiler |
| [0016](0016-preview-feature-infrastructure.md) | Preview Feature Infrastructure | Implemented | infrastructure, process |
| [0017](0017-emitter-instruction-abstraction.md) | Emitter Instruction Abstraction | Implemented | codegen, refactoring |
| [0018](0018-tracing-infrastructure.md) | Tracing Infrastructure | Implemented | infrastructure, tooling |
| [0019](0019-performance-dashboard.md) | Compiler Performance Dashboard | Implemented | tooling, website, performance |
| [0020](0020-builtin-types-as-structs.md) | Built-in Types as Synthetic Structs | Implemented | architecture, types, refactoring, strings |
| [0021](0021-stdin-input.md) | Standard Input | Implemented | io, intrinsics, runtime |
| [0022](0022-integer-parsing.md) | Integer Parsing | Implemented | intrinsics, runtime, strings |
| [0023](0023-multi-file-compilation.md) | Multi-File Compilation | Superseded | architecture, compiler, scalability |
| [0024](0024-type-intern-pool.md) | Type Intern Pool | Implemented | type-system, performance, parallelization |
| [0025](0025-comptime.md) | Compile-Time Execution (comptime) | Implemented | compiler, type-system, generics |
| [0026](0026-module-system.md) | Module System | Stable | architecture, compiler, modules, scalability |
| [0027](0027-random-intrinsics.md) | Random Number Intrinsics | Implemented | intrinsics, runtime, semantics |
| [0028](0028-unsafe-and-raw-pointers.md) | Unchecked Code and Raw Pointers | Implemented | types, semantics, stdlib, ffi |
| [0029](0029-anonymous-struct-methods.md) | Anonymous Struct Methods (Zig-Style) | Implemented | types, methods, comptime, generics |
| [0030](0030-place-expressions.md) | Place Expressions for Memory Locations | Implemented | ir, codegen, performance |
| [0031](0031-robust-performance-testing.md) | Robust Performance Testing Infrastructure | Proposal | tooling, ci, performance |
| [0032](0032-data-structure-selection.md) | Data Structure Selection for Small Collections | Implemented | performance, implementation |
| [0033](0033-sema-pipeline-unification.md) | Sema Pipeline Unification | Rejected | compiler, semantics, architecture |
| [0034](0034-cross-target-runtime.md) | Per-Target Runtime Archives for Cross-Compilation | Accepted | runtime, cross-compilation, build-system, targets |
| [0035](0035-string-model-byte-strings.md) | String model: byte strings (conventionally UTF-8) with loud pragmatism | Accepted | strings, text, unicode, stdlib |
| [0036](0036-behavior-classification-preference.md) | Behavior classification preference: prefer the most-defined category | Accepted | spec, conformance, safety, principle |
| [0037](0037-exclusivity-model-access-point-based.md) | Exclusivity model: access-point-based, statically enforced (Hylo-style) | Accepted | ownership, exclusivity, borrows, semantics, principle |
| [0038](0038-error-handling-sum-types-result-must-check.md) | Error handling: sum types, Result/Option, and must-check via linearity | Implemented | error-handling, enums, sum-types, linearity, pattern-matching |
| [0039](0039-drop-intrinsic-intentional-destroy.md) | `@drop`: the intentional-destroy intrinsic for linear (and affine) values | Accepted | linearity, ownership, intrinsics, destructors |
| [0040](0040-array-layout-ascending.md) | Array layout is ascending; @ptr_offset is standard pointer arithmetic | Implemented | layout, arrays, pointers, codegen, abi |
| [0041](0041-vec.md) | Vec — a growable collection on unchecked raw pointers | Accepted | stdlib, collections, generics, ownership, unchecked |
| [0042](0042-std-availability-model.md) | Standard-library availability model (str/String split, prelude vs explicit std) | Accepted | stdlib, modules, strings, prelude, ergonomics, language-shape |
| [0043](0043-collection-string-type-trio.md) | The collection & string type trio: fixed / slice / growable | Accepted | strings, collections, slices, arrays, vec, allocators, stdlib |
| [0044](0044-optimization-levels.md) | Optimization Levels (-O0/-O1/-O2/-O3) | Accepted | compiler, codegen, process |
| [0045](0045-lazy-semantic-analysis.md) | Lazy semantic analysis (compile-on-reference) | Accepted | compiler, semantics, comptime, modules, stdlib, language-shape |
| [0046](0046-delete-flat-mode.md) | Delete flat multi-file mode (all cross-file references go through @import) | Accepted | modules, semantics, cli, language-shape, ergonomics |
| [0047](0047-root-module-build-inputs.md) | Root-module compilation units and build-system inputs | Accepted | modules, compiler, build-system, packages, cli, language-shape |
<!-- ADR-INDEX:END -->
