# ADR-013: Constant Expression Evaluation

## Status

Accepted (Implemented)

## Context

The current `try_get_const_index` function in `rue-air/src/sema.rs` only recognizes direct integer literals and negated literals as compile-time constants:

```rust
fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
    let inst = self.rir.get(inst_ref);
    match &inst.data {
        InstData::IntConst(value) => i64::try_from(*value).ok(),
        InstData::Neg { operand } => {
            // Handle -N
            let operand_inst = self.rir.get(*operand);
            if let InstData::IntConst(value) = &operand_inst.data {
                i64::try_from(*value).ok().and_then(|v| v.checked_neg())
            } else {
                None
            }
        }
        _ => None,
    }
}
```

This means expressions like `arr[1 + 1]` or `arr[2 * 3]` don't get compile-time bounds checking, even though their values are statically known. Users expect the compiler to catch obvious out-of-bounds errors like `arr[1 + 100]` at compile time rather than waiting for a runtime panic.

Additionally, Rue may want to add a `comptime` feature in the future (similar to Zig), which would require a more robust constant evaluation infrastructure. The design of this feature should not foreclose on that possibility.

## Decision

Implement a **constant expression evaluator** at the RIR level during semantic analysis. This evaluator will:

1. Recursively evaluate RIR expressions that are compile-time determinable
2. Return `None` for expressions that require runtime values
3. Be used by bounds checking and potentially other compile-time checks

### Core Design

```rust
/// A value that can be computed at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstValue {
    /// Integer value (signed to handle arithmetic correctly)
    Integer(i64),
    /// Boolean value
    Bool(bool),
}

impl Sema<'_> {
    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls, or operations that would overflow/panic).
    fn try_evaluate_const(&self, inst_ref: InstRef) -> Option<ConstValue> {
        // ...
    }
}
```

### Supported Expressions (Phase 1)

The initial implementation will evaluate:

| Expression | Example | Notes |
|------------|---------|-------|
| Integer literals | `42` | Direct constant |
| Boolean literals | `true`, `false` | Direct constant |
| Negation | `-42` | Unary minus on constant |
| Logical NOT | `!true` | Returns `false` |
| Addition | `1 + 2` | Checked arithmetic |
| Subtraction | `5 - 3` | Checked arithmetic |
| Multiplication | `2 * 3` | Checked arithmetic |
| Division | `6 / 2` | Returns `None` if divisor is 0 |
| Modulo | `7 % 3` | Returns `None` if divisor is 0 |
| Comparisons | `1 < 2` | Returns `ConstValue::Bool` |
| Logical AND/OR | `true && false` | Short-circuit not needed for constants |
| Parentheses | `(1 + 2) * 3` | Handled by recursion |

### Expressions That Return None

- Variable references (`x`, `arr[i]`)
- Function calls
- Operations that would overflow (e.g., `i64::MAX + 1`)
- Operations that would panic (e.g., `1 / 0`)
- Struct/array literals (for now)

### Usage for Bounds Checking

```rust
fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
    match self.try_evaluate_const(inst_ref) {
        Some(ConstValue::Integer(n)) => Some(n),
        _ => None,
    }
}
```

## Why This Design Supports Future `comptime`

### 1. Separate Evaluation from IR Lowering

The `try_evaluate_const` function evaluates expressions without modifying the IR. This separation is important because:

- **Current use**: We evaluate to check bounds, but still emit the original arithmetic as IR
- **Future `comptime`**: We would evaluate AND replace the expression with its result in the IR

When `comptime` is added, the infrastructure can be extended:

```rust
// Future: comptime-aware analysis
fn analyze_inst(&mut self, inst_ref: InstRef, ctx: &mut AnalysisContext) -> CompileResult<AirRef> {
    // If in comptime context and expression is const-evaluable, emit constant
    if ctx.is_comptime {
        if let Some(value) = self.try_evaluate_const(inst_ref) {
            return self.emit_const_value(value);
        } else {
            return Err(CompileError::new(
                ErrorKind::NotComptime,
                self.rir.get(inst_ref).span,
            ));
        }
    }
    // Otherwise, normal analysis
    // ...
}
```

### 2. Extensible ConstValue Enum

The `ConstValue` type can grow to support more compile-time values:

```rust
// Future extension for comptime
pub enum ConstValue {
    Integer(i64),
    Bool(bool),
    Unit,
    // Future additions:
    Array(Vec<ConstValue>),
    Struct { fields: Vec<ConstValue> },
    // For comptime function evaluation:
    Function(FunctionId),
}
```

### 3. Clear Const vs Runtime Boundary

The `Option<ConstValue>` return type explicitly marks the boundary between what can and cannot be evaluated at compile time. This aligns with `comptime` semantics where:

- Using a runtime value in a `comptime` context is an error
- The evaluator returns `None` precisely when this boundary is crossed

### 4. No Premature Constant Folding

We intentionally do NOT replace all constant expressions with their values in the IR. This is important because:

- It preserves the original source structure for debugging and error messages
- It leaves room for `comptime` to have explicit semantics about when folding occurs
- It avoids the question of whether `1 + 1` should become `2` in the IR (a policy decision for later)

## Alternatives Considered

### Alternative 1: Constant Folding as an Optimization Pass

Instead of evaluating constants during semantic analysis, we could add a separate constant folding pass over the AIR or CFG.

**Rejected because:**
- Bounds checking happens during semantic analysis, so we need constant values at that stage
- Adding a separate pass would complicate the pipeline
- The evaluator needs access to type information that's available during sema

### Alternative 2: Track "Constness" as a Type Property

We could add a `const` modifier to types, so `1 + 2` would have type `const i32`.

**Rejected because:**
- This is a larger change that affects the entire type system
- It's the right design for full `const fn` support but overkill for bounds checking
- We can add this later when implementing `comptime` without changing the evaluator

### Alternative 3: Only Extend try_get_const_index

We could keep the current pattern but add more cases:

```rust
fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
    match &inst.data {
        InstData::IntConst(v) => Some(*v as i64),
        InstData::Neg { operand } => self.try_get_const_index(*operand).map(|v| -v),
        InstData::Add { lhs, rhs } => {
            let l = self.try_get_const_index(*lhs)?;
            let r = self.try_get_const_index(*rhs)?;
            l.checked_add(r)
        }
        // ... more cases
        _ => None,
    }
}
```

**Rejected because:**
- Duplicates logic if we need constant evaluation elsewhere (e.g., for const item values)
- Mixes integer-specific logic with general constant evaluation
- Harder to extend to booleans and other types

## Implementation Plan

1. **Add `ConstValue` enum** in `rue-air/src/sema.rs`
2. **Implement `try_evaluate_const`** with cases for all arithmetic/comparison/logical operators
3. **Refactor `try_get_const_index`** to use `try_evaluate_const`
4. **Update the specification** to document constant expression evaluation
5. **Add spec tests** for constant-folded bounds checking

### Files to Modify

| File | Change |
|------|--------|
| `crates/rue-air/src/sema.rs` | Add `ConstValue`, `try_evaluate_const` |
| `docs/spec/src/08-runtime-behavior/02-bounds-checking.md` | Document constant expression evaluation |
| `crates/rue-spec/cases/runtime/bounds.toml` | Add tests for `arr[1 + 1]`, etc. |

## Consequences

### Positive

- **Better compile-time error detection**: Catches more bounds errors at compile time
- **Foundation for `comptime`**: The evaluator can be extended for full compile-time execution
- **Clean separation**: Evaluation logic is isolated and testable
- **Predictable behavior**: Overflow/division-by-zero during evaluation returns `None` (runtime check)

### Negative

- **Incomplete evaluation**: Not all mathematically constant expressions are recognized (e.g., `x` where `x` is assigned a constant but never mutated)
- **Recursion depth**: Deeply nested expressions could cause stack overflow (unlikely in practice)

### Neutral

- **No runtime behavior change**: This only affects when errors are reported, not what errors occur
- **Consistent with Rust**: Rust's const evaluation has similar limitations before `const fn`

## Future Work

When implementing `comptime`, the following extensions would be needed:

1. **`const` items**: `const FOO: i32 = 1 + 2;`
2. **`comptime` blocks**: `comptime { complex_computation() }`
3. **`const fn`**: Functions that can be evaluated at compile time
4. **Compile-time function evaluation**: Call `const fn` during compilation
5. **Type-level constants**: For const generics like `[T; N]`

This ADR specifically avoids these to keep the scope minimal while establishing the foundation.

## References

- [Zig's comptime](https://ziglang.org/documentation/master/#comptime)
- [Rust's const evaluation](https://doc.rust-lang.org/reference/const_eval.html)
- Issue: tree1-dzn "Extend compile-time bounds checking to handle constant-foldable index expressions"
