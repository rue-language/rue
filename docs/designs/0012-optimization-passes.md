---
id: 0012
title: Compiler Optimization Passes
status: proposal
tags: [compiler, codegen]
feature-flag: none
created: 2025-12-25
accepted:
implemented:
spec-sections: []
superseded-by:
---

<!-- Note: Optimizations are compiler internals that don't affect language semantics.
     No preview feature gate is needed - optimizations don't change what programs mean,
     only how efficiently they execute. -->

# ADR-0012: Compiler Optimization Passes

## Status

Proposal

## Summary

Add optimization passes to the Rue compiler operating at the CFG level. Initial passes include constant folding (at CFG level to complement existing RIR-level evaluation) and dead code elimination. These improve codegen quality without changing program semantics. Optimization levels follow standard compiler conventions (`-O0` through `-O3`).

## Context

Currently, the Rue compiler has minimal optimization:

1. **Constant evaluation at RIR level** (ADR-0003): `try_evaluate_const()` in sema.rs folds arithmetic on literal constants during semantic analysis. This handles `let x = 2 + 3` by computing 5 at compile time.

2. **No CFG-level optimization**: After CFG construction, no simplification occurs before lowering to MIR.

This means:
- Dead stores (variables written but never read) are still computed and stored
- Unreachable code after unconditional returns is still emitted

While these don't affect correctness, they produce suboptimal machine code. For a systems language targeting performance-conscious users, better codegen quality is expected.

### Current Pipeline

```
Source -> Lexer -> Parser -> AstGen -> Sema -> CfgBuilder -> CfgLower -> RegAlloc -> Emit
                                        |           |            |
                                   try_evaluate_  Creates      Lowers to
                                   const (RIR)    CFG          MIR
```

### Proposed Pipeline

```
Source -> Lexer -> Parser -> AstGen -> Sema -> CfgBuilder -> [Optimize] -> CfgLower -> RegAlloc -> Emit
                                        |           |             |            |
                                   try_evaluate_  Creates     CFG->CFG     Lowers to
                                   const (RIR)    CFG        transforms    MIR
```

## Decision

Add a CFG optimization pass that runs after CFG construction and before lowering to MIR. The pass operates as CFG -> CFG transformations, preserving the CFG structure while simplifying instructions.

### Architecture

Create a new module `crates/rue-cfg/src/opt/` with:

```
rue-cfg/src/opt/
├── mod.rs           # Optimization pipeline orchestration
├── constfold.rs     # Constant folding pass
└── dce.rs           # Dead code elimination
```

### Pass 1: Constant Folding (constfold.rs)

Fold operations on constants that weren't caught at RIR level:

```rust
// Before optimization:
v0 = const 5
v1 = const 3
v2 = add v0, v1

// After optimization:
v0 = const 5      // May be DCE'd if unused
v1 = const 3      // May be DCE'd if unused
v2 = const 8      // Folded result
```

**Foldable operations:**
- Arithmetic: Add, Sub, Mul, Div, Mod (with overflow/div-zero checks)
- Comparisons: Eq, Ne, Lt, Gt, Le, Ge
- Bitwise: BitAnd, BitOr, BitXor, Shl, Shr
- Unary: Neg, Not, BitNot

**Why CFG-level when RIR already folds?**
- RIR folding catches literal expressions (`2 + 3`)
- CFG folding catches patterns that emerge after CFG construction
- Block parameters (phi-like values) with constant arguments
- Future: interprocedural constants after inlining

### Pass 2: Dead Code Elimination (dce.rs)

Remove unused values and unreachable blocks:

**Value-level DCE:**
```rust
// Before:
v0 = const 42
v1 = alloc 0, v0   // Store to slot 0
v2 = const 10
ret v2             // v1 (slot 0) never read

// After:
v2 = const 10
ret v2             // Dead store eliminated
```

**Block-level DCE:**
```rust
// Before:
bb0:
  ret v0
bb1:                // Unreachable (no predecessors)
  v1 = const 42
  goto bb2

// After:
bb0:
  ret v0
// bb1 removed
```

**Algorithm:**
1. Compute liveness: mark values reachable from terminators
2. Mark side-effecting instructions as live (calls, stores to escaping slots)
3. Remove unmarked instructions
4. Remove blocks with no predecessors (except entry)

### Optimization Levels

Follow standard compiler conventions:

| Level | Flag | Behavior |
|-------|------|----------|
| 0 | `-O0` | No optimization (default for now) |
| 1 | `-O1` | Basic optimizations (constant folding, DCE) |
| 2 | `-O2` | All optimizations (same as -O1 for now) |
| 3 | `-O3` | Aggressive optimizations (same as -O2 for now) |

Future optimization passes will be added at appropriate levels.

### API

```rust
// In rue-cfg/src/opt/mod.rs

/// Optimization level, following standard compiler conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization (-O0)
    #[default]
    O0,
    /// Basic optimizations (-O1): constant folding, DCE
    O1,
    /// Standard optimizations (-O2): all of O1
    O2,
    /// Aggressive optimizations (-O3): all of O2
    O3,
}

/// Run optimization passes on a CFG at the given level.
pub fn optimize(cfg: &mut Cfg, level: OptLevel) {
    match level {
        OptLevel::O0 => {
            // No optimization
        }
        OptLevel::O1 | OptLevel::O2 | OptLevel::O3 => {
            constfold::run(cfg);
            dce::run(cfg);
        }
    }
}
```

### Integration

In `rue-compiler/src/lib.rs`, add optimization after CFG building:

```rust
// Build CFG
let cfg = CfgBuilder::new(&air, &struct_defs, &array_types).build()?;

// Optimize based on -O level
let mut cfg = cfg;
rue_cfg::opt::optimize(&mut cfg, options.opt_level);

// Lower to MIR
let mir = CfgLower::new(&cfg, ...).lower()?;
```

### Testing Strategy

Optimizations don't change program semantics, so existing spec tests continue to pass. For testing the optimization passes themselves:

1. **Functional tests (spec tests)**: All existing tests remain valid - programs produce the same results.

2. **Golden tests (IR dumps)**: Update `crates/rue-spec/cases/golden/ir-dumps.toml` to reflect optimized CFG/MIR output. Add new cases specifically for optimization.

3. **UI tests**: Add `crates/rue-ui-tests/cases/optimization/` for:
   - Verifying specific optimizations trigger
   - Testing optimization flags (`-O0`, `-O1`, etc.)
   - Regression tests for miscompilations

Example UI test:
```toml
[[case]]
name = "constant_folding_at_O1"
opt_level = 1
source = """
fn main() -> i32 {
    let x = 2 + 3;
    x
}
"""
exit_code = 5
cfg_contains = ["const 5"]      # Verify folded constant
cfg_not_contains = ["add"]      # Verify no add instruction
```

### Compiler Flags

Add `-O` flags to control optimization level:

- `-O0`: No optimization (default)
- `-O1`: Basic optimizations (constant folding, DCE)
- `-O2`: Standard optimizations (same as -O1 for now)
- `-O3`: Aggressive optimizations (same as -O2 for now)

Tests can specify `opt_level = N` to control which level is used for that test case.

### Multi-Backend Considerations

CFG optimization is target-independent - it operates on typed CFG instructions before lowering to architecture-specific MIR. The optimization passes work identically for x86_64 and aarch64.

## Implementation Phases

- [x] **Phase 1: Optimization framework** - rue-aapc.1
  - Create `rue-cfg/src/opt/mod.rs` with `OptLevel` enum and pass orchestration
  - Add `-O0` through `-O3` flags to CLI
  - Add UI test infrastructure with `opt_level` support
  - Wire up in rue-compiler

- [ ] **Phase 2: Constant folding** - rue-aapc.2
  - Implement `constfold.rs`
  - Add tests verifying folding occurs at -O1+
  - Update golden tests as needed

- [ ] **Phase 3: Dead code elimination** - rue-aapc.3
  - Implement `dce.rs` with liveness analysis
  - Handle side-effects correctly (calls, escaping stores)
  - Add tests for dead store/block elimination

## Consequences

### Positive

- **Better code quality**: Obvious inefficiencies are eliminated
- **Foundation for future optimization**: Framework supports adding more passes
- **Predictable behavior**: Optimizations are conservative and well-defined
- **Debuggability**: `-O0` preserves direct correspondence to source
- **Standard interface**: `-O` levels familiar to developers from gcc/clang

### Negative

- **Golden test maintenance**: IR dump tests need updating when optimization changes
- **Compilation time**: Minor increase (optimization passes take time)
- **Debugging complexity**: Optimized code may differ from source structure

### Neutral

- **No semantic changes**: All existing tests pass unchanged
- **No preview gate needed**: Optimizations don't change language behavior

## Open Questions

None remaining - decisions made:
- Optimization levels use standard `-O0` through `-O3` flags
- No diagnostics emitted for optimizations
- Strength reduction deferred to future work

## Future Work

- **Strength reduction**: Multiply/divide by power of 2 -> shifts
- **Loop optimizations**: Unrolling, invariant hoisting
- **Inlining**: For small functions
- **Common subexpression elimination**: Reuse computed values
- **Register allocation hints**: From optimization analysis
- **Profile-guided optimization**: Hot path optimization

## References

- [ADR-0003: Constant Expression Evaluation](0003-constant-evaluation.md) - Existing RIR-level folding
- [Engineering a Compiler, Chapter 8](https://www.elsevier.com/books/engineering-a-compiler/cooper/978-0-12-815412-0) - Optimization theory
- [LLVM Optimization Passes](https://llvm.org/docs/Passes.html) - Reference for pass design
