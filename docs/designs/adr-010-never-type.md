# ADR-010: The Never Type (!)

## Status

Accepted (Implemented)

## Context

Rue currently treats `break` and `continue` as statements that produce `Unit` type. This is limiting because it prevents patterns like:

```rue
let y = if condition { break } else { x };
```

In languages like Rust, this works because `break` has type `!` (never), which can coerce to any type. The issue description states: "we have no ! type, which means break/continue are statements rather than expressions, this is less than ideal."

## Decision

Add a `Never` type to Rue's type system that represents computations that never produce a value (they diverge). The never type can coerce to any other type during type unification.

### Type System Changes

#### 1. Extend the Type enum

In [rue-air/src/types.rs](../../crates/rue-air/src/types.rs):

```rust
pub enum Type {
    // ... existing variants ...

    /// The never type - represents computations that don't return.
    /// Can coerce to any other type.
    Never,
}
```

#### 2. Add helper methods

```rust
impl Type {
    /// Check if this is the never type.
    pub fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    /// Check if this type can coerce to the target type.
    /// Never can coerce to anything. Error can coerce to anything (for error recovery).
    pub fn can_coerce_to(&self, target: &Type) -> bool {
        self.is_never() || self.is_error() || self == target
    }
}
```

#### 3. Update type name display

```rust
impl Type {
    pub fn name(&self) -> &'static str {
        match self {
            // ... existing ...
            Type::Never => "!",
        }
    }
}
```

### Semantic Analysis Changes

The key insight is that the current type unification uses strict equality (`==`). We need to change it to use **subsumption** where `Never` can satisfy any expected type.

#### 1. Update branch type unification

In [rue-air/src/sema.rs](../../crates/rue-air/src/sema.rs), the branch handling (around line 497-561) currently does:

```rust
// Current code
if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
    return Err(TypeMismatch { ... });
}
```

Change to:

```rust
// Compute the unified result type
let result_type = match (then_type.is_never(), else_type.is_never()) {
    (true, true) => Type::Never,      // Both diverge -> Never
    (true, false) => else_type,        // Then diverges -> use else type
    (false, true) => then_type,        // Else diverges -> use then type
    (false, false) => {
        // Neither diverges - types must match exactly
        if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
            return Err(TypeMismatch { ... });
        }
        then_type
    }
};
```

#### 2. Update break/continue typing

Currently break and continue return `Unit`. Change them to return `Never`:

```rust
InstData::Break => {
    if ctx.loop_depth == 0 {
        return Err(BreakOutsideLoop);
    }
    Ok(air.add_inst(AirInst {
        data: AirInstData::Break,
        ty: Type::Never,  // Changed from Type::Unit
        span: inst.span,
    }))
}

InstData::Continue => {
    if ctx.loop_depth == 0 {
        return Err(ContinueOutsideLoop);
    }
    Ok(air.add_inst(AirInst {
        data: AirInstData::Continue,
        ty: Type::Never,  // Changed from Type::Unit
        span: inst.span,
    }))
}
```

#### 3. Update expected type checking

Throughout `analyze_inst`, there are checks like:

```rust
if ty != expected_type && expected_type != Type::Unit && !expected_type.is_error() {
    return Err(TypeMismatch { ... });
}
```

These need to also allow `Never`:

```rust
if !ty.can_coerce_to(&expected_type) && expected_type != Type::Unit {
    return Err(TypeMismatch { ... });
}
```

Where `can_coerce_to` handles the `Never` -> any, `Error` -> any, and exact match cases.

#### 4. Update infer_type

The `infer_type` function should return `Never` for break/continue:

```rust
InstData::Break | InstData::Continue => Ok(Type::Never),
```

### Code Generation Changes

Code generation ([rue-codegen/src/lib.rs](../../crates/rue-codegen/src/lib.rs)) should handle `Never` type gracefully:

1. **No value to move**: When lowering a `Never`-typed instruction to a register, it's unreachable code. The existing break/continue handling already emits a jump, so the "value" is never actually used.

2. **Branch lowering**: When lowering a branch where one side is `Never`, the result register is only written by the non-diverging side.

The existing code gen likely needs minimal changes because:
- Break/continue already emit jumps (no value produced)
- The "result" of a `Never`-typed expression is never actually read
- The AIR already contains the control flow structure

### Lexer/Parser Changes

**None required.** We're not adding `!` as a user-writable type annotation (yet). The never type is inferred from control flow constructs. Future work could add:
- `fn diverges() -> !` syntax
- `loop { }` keyword that has type `!`

### Grammar Changes

**None required** for this phase. The grammar already supports break/continue as primary expressions. They simply weren't being used in expression contexts due to type system limitations.

## Phases of Implementation

### Phase 1: Core Never Type (This ADR)

1. Add `Type::Never` variant
2. Update `break`/`continue` to produce `Never`
3. Update branch unification to handle `Never`
4. Update type checking to allow `Never` coercion

### Phase 2: Future Extensions (Separate ADRs)

- `loop { }` keyword (infinite loop with type `!`)
- `panic!()` or similar (function that returns `!`)
- `-> !` return type annotation
- Dead code detection after diverging expressions
- `return` statement (returns `!` to caller)

## File Changes Summary

| File | Changes |
|------|---------|
| `crates/rue-air/src/types.rs` | Add `Never` variant, `is_never()`, `can_coerce_to()` |
| `crates/rue-air/src/sema.rs` | Update branch unification, break/continue typing, type checks |
| `crates/rue-spec/cases/14-never-type.toml` | New spec file with test cases |
| `docs/language.md` | Update documentation |

## Consequences

### Positive

- **More expressive**: Enables `let x = if cond { break } else { value };` patterns
- **Rust familiarity**: Matches Rust's type system behavior
- **Foundation**: Enables future features like `loop`, `panic!`, `return`
- **Better inference**: Type inference uses non-diverging branch

### Negative

- **Complexity**: Type system moves from equality to subsumption (though localized)
- **Learning curve**: Users must understand why `break` can appear in value position

### Neutral

- **No syntax changes**: Users don't write `!` themselves (in this phase)
- **Backwards compatible**: All existing code continues to work

## Test Plan

The specification file `crates/rue-spec/cases/14-never-type.toml` contains comprehensive tests:

1. **Basic break/continue in branches** (8 tests)
2. **Nested if expressions** (2 tests)
3. **Both branches diverge** (2 tests)
4. **Arithmetic expressions** (2 tests)
5. **Function arguments** (2 tests)
6. **Variable assignment** (2 tests)
7. **Chained if-else** (3 tests)
8. **Complex control flow** (3 tests)
9. **Error cases** (3 tests)
10. **Edge cases** (5 tests)

Run with: `./buck2 run //crates/rue-spec:rue-spec -- "14.1"`

## Acceptance Criteria

- [ ] All tests in `14-never-type.toml` pass
- [ ] Existing tests continue to pass (`./test.sh`)
- [ ] `break` and `continue` can appear in expression position inside loops
- [ ] Type inference correctly uses the non-diverging branch type
- [ ] Error messages for type mismatches are still clear
- [ ] `break`/`continue` outside loops still produce clear errors
