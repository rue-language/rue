# ADR-012: Single-Pass Bidirectional Type Checking

## Status

Accepted (Implemented)

## Context

The current semantic analysis in `rue-air/src/sema.rs` traverses the RIR multiple times:

1. **`infer_type()`** walks the tree to determine a type bottom-up
2. **`analyze_inst()`** walks the same tree again to emit AIR instructions

This happens in several places:
- `Alloc` without type annotation (line 713): infer initializer type, then analyze it
- `Block` with Unit context (line 954): infer last expression type, then analyze it
- `FieldGet` (line 1176): infer base type, then analyze base
- `analyze_comparison` (line 1439): infer LHS type, then analyze both operands

The redundancy comes from bidirectional type checking: we need to know a type before we can check against it, but determining the type requires traversing the subtree.

## Decision

Refactor `analyze_inst` to use a **synthesize/check** pattern where:
- **Synthesize mode**: Infer the type bottom-up, emit AIR, return both
- **Check mode**: Verify against expected type top-down, emit AIR

This eliminates the separate `infer_type()` traversal by having `analyze_inst` always return the synthesized type along with the AIR reference.

## Prototype

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

### Example: Integer Constant

**Before:**
```rust
InstData::IntConst(value) => {
    let ty = if expected_type.is_integer() {
        expected_type
    } else {
        Type::I32
    };

    if ty != expected_type && expected_type != Type::Unit && !expected_type.is_error() {
        return Err(CompileError::new(...));
    }

    Ok(air.add_inst(AirInst {
        data: AirInstData::Const(*value),
        ty,
        span: inst.span,
    }))
}
```

**After:**
```rust
InstData::IntConst(value) => {
    let ty = expectation.integer_type();
    expectation.check(ty, inst.span)?;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Const(*value),
        ty,
        span: inst.span,
    });

    Ok(AnalysisResult { air_ref, ty })
}
```

### Example: Comparison Operators (the main win)

**Before (two traversals):**
```rust
fn analyze_comparison<F>(
    &mut self,
    air: &mut Air,
    lhs: InstRef,
    rhs: InstRef,
    allow_bool: bool,
    make_data: F,
    span: Span,
    ctx: &mut AnalysisContext,
) -> CompileResult<AirRef>
where
    F: FnOnce(AirRef, AirRef) -> AirInstData,
{
    // TRAVERSAL 1: infer_type walks the LHS subtree
    let lhs_type = self.infer_type(lhs, &ctx.locals, ctx.params)?;

    // Validate type...
    if allow_bool {
        if !lhs_type.is_integer() && lhs_type != Type::Bool {
            return Err(...);
        }
    } else if !lhs_type.is_integer() {
        return Err(...);
    }

    // TRAVERSAL 2: analyze_inst walks the LHS subtree AGAIN
    let lhs_ref = self.analyze_inst(air, lhs, lhs_type, ctx)?;
    let rhs_ref = self.analyze_inst(air, rhs, lhs_type, ctx)?;

    Ok(air.add_inst(AirInst {
        data: make_data(lhs_ref, rhs_ref),
        ty: Type::Bool,
        span,
    }))
}
```

**After (single traversal):**
```rust
fn analyze_comparison<F>(
    &mut self,
    air: &mut Air,
    lhs: InstRef,
    rhs: InstRef,
    allow_bool: bool,
    make_data: F,
    span: Span,
    ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult>
where
    F: FnOnce(AirRef, AirRef) -> AirInstData,
{
    // SINGLE TRAVERSAL: synthesize type AND emit AIR in one pass
    let lhs_result = self.analyze_inst(air, lhs, TypeExpectation::Synthesize, ctx)?;
    let lhs_type = lhs_result.ty;

    // Propagate Never/Error without additional type errors
    if lhs_type.is_never() || lhs_type.is_error() {
        let rhs_result = self.analyze_inst(air, rhs, TypeExpectation::Check(Type::I32), ctx)?;
        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::Bool,
            span,
        });
        return Ok(AnalysisResult { air_ref, ty: Type::Bool });
    }

    // Validate the type is appropriate for this comparison
    if allow_bool {
        if !lhs_type.is_integer() && lhs_type != Type::Bool {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer or bool".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }
    } else if !lhs_type.is_integer() {
        return Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "integer".to_string(),
                found: lhs_type.name().to_string(),
            },
            self.rir.get(lhs).span,
        ));
    }

    // RHS is checked against synthesized LHS type
    let rhs_result = self.analyze_inst(air, rhs, TypeExpectation::Check(lhs_type), ctx)?;

    let air_ref = air.add_inst(AirInst {
        data: make_data(lhs_result.air_ref, rhs_result.air_ref),
        ty: Type::Bool,
        span,
    });

    Ok(AnalysisResult { air_ref, ty: Type::Bool })
}
```

### Example: Alloc (let binding without type annotation)

**Before (two traversals):**
```rust
InstData::Alloc { name, is_mut, ty, init } => {
    let var_type = if let Some(type_sym) = ty {
        self.resolve_type(*type_sym, inst.span)?
    } else {
        // TRAVERSAL 1: infer_type walks the initializer
        self.infer_type(*init, &ctx.locals, ctx.params)?
    };

    // TRAVERSAL 2: analyze_inst walks the initializer AGAIN
    let init_ref = self.analyze_inst(air, *init, var_type, ctx)?;
    // ...
}
```

**After (single traversal):**
```rust
InstData::Alloc { name, is_mut, ty, init } => {
    let (init_result, var_type) = if let Some(type_sym) = ty {
        // Type annotation provided: check initializer against it
        let var_type = self.resolve_type(*type_sym, inst.span)?;
        let init_result = self.analyze_inst(air, *init, TypeExpectation::Check(var_type), ctx)?;
        (init_result, var_type)
    } else {
        // No annotation: synthesize type from initializer (SINGLE TRAVERSAL)
        let init_result = self.analyze_inst(air, *init, TypeExpectation::Synthesize, ctx)?;
        (init_result, init_result.ty)
    };

    // Allocate slots...
    let slot = ctx.next_slot;
    // ...

    Ok(AnalysisResult {
        air_ref: air.add_inst(AirInst {
            data: AirInstData::Alloc { slot, init: init_result.air_ref },
            ty: Type::Unit,
            span: inst.span,
        }),
        ty: Type::Unit,
    })
}
```

### Example: FieldGet

**Before (two traversals):**
```rust
InstData::FieldGet { base, field } => {
    // TRAVERSAL 1: infer base type
    let base_type = self.infer_type(*base, &ctx.locals, ctx.params)?;

    let struct_id = match base_type { ... };
    let field_type = /* lookup field */;

    // Type check against expected...

    // TRAVERSAL 2: analyze base
    let base_ref = self.analyze_inst(air, *base, base_type, ctx)?;

    Ok(air.add_inst(AirInst {
        data: AirInstData::FieldGet { base: base_ref, struct_id, field_index },
        ty: field_type,
        span: inst.span,
    }))
}
```

**After (single traversal):**
```rust
InstData::FieldGet { base, field } => {
    // SINGLE TRAVERSAL: synthesize base type AND emit AIR
    let base_result = self.analyze_inst(air, *base, TypeExpectation::Synthesize, ctx)?;
    let base_type = base_result.ty;

    let struct_id = match base_type {
        Type::Struct(id) => id,
        _ => return Err(CompileError::new(
            ErrorKind::FieldAccessOnNonStruct { found: base_type.name().to_string() },
            inst.span,
        )),
    };

    let struct_def = &self.struct_defs[struct_id.0 as usize];
    let field_name_str = self.interner.get(*field).to_string();
    let (field_index, struct_field) = struct_def.find_field(&field_name_str).ok_or_else(|| ...)?;
    let field_type = struct_field.ty;

    // Check against expectation
    expectation.check(field_type, inst.span)?;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::FieldGet {
            base: base_result.air_ref,
            struct_id,
            field_index: field_index as u32,
        },
        ty: field_type,
        span: inst.span,
    });

    Ok(AnalysisResult { air_ref, ty: field_type })
}
```

### Example: Block with Unit Context

**Before:**
```rust
let inst_expected_type = if is_last {
    if expected_type == Type::Unit {
        // EXTRA TRAVERSAL: infer type when in Unit context
        self.infer_type(inst_ref, &ctx.locals, ctx.params)?
    } else {
        expected_type
    }
} else {
    Type::Unit
};
let air_ref = self.analyze_inst(air, inst_ref, inst_expected_type, ctx)?;
```

**After:**
```rust
let inst_expectation = if is_last {
    if matches!(expectation, TypeExpectation::Check(Type::Unit)) {
        // In Unit context, synthesize type (don't enforce Unit on final expr)
        TypeExpectation::Synthesize
    } else {
        expectation
    }
} else {
    TypeExpectation::Check(Type::Unit)
};
let result = self.analyze_inst(air, inst_ref, inst_expectation, ctx)?;
```

## Migration Strategy

1. **Add `AnalysisResult` struct** alongside existing code
2. **Add `TypeExpectation` enum** with helper methods
3. **Create `analyze_inst_v2`** with new signature, implementing cases one at a time
4. **Migrate callers** to use new API
5. **Delete `infer_type()`** once all callers migrated
6. **Rename `analyze_inst_v2` to `analyze_inst`**

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

### Neutral

- **Code size**: Similar total lines, just reorganized
- **Complexity**: Same fundamental algorithm, different structure

## References

- [Bidirectional Typing](https://arxiv.org/abs/1908.05839) - Dunfield & Krishnaswami
- Rust's type inference uses a similar synthesize/check pattern
- Swift's type checker explicitly uses bidirectional inference
