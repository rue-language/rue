---
id: 0003
title: Constant Expression Evaluation
status: implemented
tags: [compiler]
feature-flag: const-eval
created: 2025-01-01
accepted: 2025-01-01
implemented: 2025-01-01
spec-sections: []
superseded-by:
---

<!-- Note: This ADR predates the preview feature system (ADR-0005). The feature-flag
     is a placeholder to satisfy the schema; this feature was not actually gated. -->

# ADR-0003: Constant Expression Evaluation

## Status

Implemented

## Summary

Implement a constant expression evaluator at the RIR level during semantic analysis, enabling compile-time bounds checking for expressions like `arr[1 + 1]`.

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

### Supported Expressions

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

## Implementation Phases

- [x] **Phase 1: Core evaluator** - Add ConstValue, try_evaluate_const, refactor try_get_const_index

## Consequences

### Positive

- **Better compile-time error detection**: Catches more bounds errors at compile time
- **Foundation for `comptime`**: The evaluator can be extended for full compile-time execution
- **Clean separation**: Evaluation logic is isolated and testable
- **Predictable behavior**: Overflow/division-by-zero during evaluation returns `None` (runtime check)

### Negative

- **Incomplete evaluation**: Not all mathematically constant expressions are recognized (e.g., `x` where `x` is assigned a constant but never mutated)
- **Recursion depth**: Deeply nested expressions could cause stack overflow (unlikely in practice)

## Open Questions

None remaining.

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
