---
id: 0002
title: Single-Pass Bidirectional Type Checking
status: implemented
tags: [compiler]
feature-flag: bidirectional-types
created: 2025-01-01
accepted: 2025-01-01
implemented: 2025-01-01
spec-sections: []
superseded-by:
---

<!-- Note: This ADR predates the preview feature system (ADR-0005). The feature-flag
     is a placeholder to satisfy the schema; this feature was not actually gated. -->

# ADR-0002: Single-Pass Bidirectional Type Checking

## Status

Implemented

## Summary

Refactor semantic analysis to use a synthesize/check pattern, eliminating redundant tree traversals where `infer_type()` and `analyze_inst()` walk the same subtree separately.

## Context

The current semantic analysis in `rue-air/src/sema.rs` traverses the RIR multiple times:

1. **`infer_type()`** walks the tree to determine a type bottom-up
2. **`analyze_inst()`** walks the same tree again to emit AIR instructions

This happens in several places:
- `Alloc` without type annotation: infer initializer type, then analyze it
- `Block` with Unit context: infer last expression type, then analyze it
- `FieldGet`: infer base type, then analyze base
- `analyze_comparison`: infer LHS type, then analyze both operands

The redundancy comes from bidirectional type checking: we need to know a type before we can check against it, but determining the type requires traversing the subtree.

## Decision

Refactor `analyze_inst` to use a **synthesize/check** pattern where:
- **Synthesize mode**: Infer the type bottom-up, emit AIR, return both
- **Check mode**: Verify against expected type top-down, emit AIR

This eliminates the separate `infer_type()` traversal by having `analyze_inst` always return the synthesized type along with the AIR reference.

### Core Type: TypeExpectation

```rust
/// Describes what type we expect from an expression.
#[derive(Debug, Clone, Copy)]
enum TypeExpectation {
    /// We have a specific type we're checking against (top-down).
    /// The expression MUST have this type or be coercible to it.
    Check(Type),

    /// We don't know the type yet - synthesize it (bottom-up).
    /// The expression determines its own type.
    Synthesize,
}

impl TypeExpectation {
    /// Get the type to use for integer literals.
    fn integer_type(&self) -> Type {
        match self {
            TypeExpectation::Check(ty) if ty.is_integer() => *ty,
            _ => Type::I32,
        }
    }

    /// Check if a synthesized type is compatible with this expectation.
    fn check(&self, synthesized: Type, span: Span) -> CompileResult<()> {
        match self {
            TypeExpectation::Synthesize => Ok(()),
            TypeExpectation::Check(expected) => {
                if synthesized == *expected
                    || *expected == Type::Unit  // Unit context accepts anything
                    || synthesized.is_never()   // Never coerces to anything
                    || expected.is_error()
                    || synthesized.is_error()
                {
                    Ok(())
                } else {
                    Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected.name().to_string(),
                            found: synthesized.name().to_string(),
                        },
                        span,
                    ))
                }
            }
        }
    }
}
```

### Modified analyze_inst Signature

```rust
/// Result of analyzing an instruction: the AIR reference and its type.
struct AnalysisResult {
    air_ref: AirRef,
    ty: Type,
}

/// Analyze an RIR instruction, producing AIR instructions.
///
/// Returns both the AIR reference and the synthesized type.
/// When `expectation` is `Check(ty)`, validates that the result is compatible.
fn analyze_inst(
    &mut self,
    air: &mut Air,
    inst_ref: InstRef,
    expectation: TypeExpectation,
    ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult>
```

## Implementation Phases

- [x] **Phase 1: Core refactor** - Add TypeExpectation, AnalysisResult, migrate analyze_inst

## Consequences

### Positive

- **Eliminates duplicate traversals**: Each subtree is visited exactly once
- **Cleaner code**: Type synthesis and checking unified in one place
- **Better error messages**: We have full context when errors occur
- **Foundation for type inference**: This pattern scales to Hindley-Milner style inference
- **Performance**: Fewer function calls, better cache locality

### Negative

- **Larger return type**: Every call returns `(AirRef, Type)` instead of just `AirRef`
- **Migration effort**: Significant refactor of ~1400 lines in sema.rs
- **Testing**: Need comprehensive tests to ensure behavior unchanged

## Open Questions

None remaining.

## Future Work

- Full Hindley-Milner type inference if needed for generics

## References

- [Bidirectional Typing](https://arxiv.org/abs/1908.05839) - Dunfield & Krishnaswami
- Rust's type inference uses a similar synthesize/check pattern
- Swift's type checker explicitly uses bidirectional inference
