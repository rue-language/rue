---
id: 0015
title: Test Suite Optimization
status: proposal
tags: [process, testing, compiler]
feature-flag: null
created: 2025-12-26
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0015: Test Suite Optimization

## Status

Proposal

## Summary

Optimize the Rue test suite for faster development iteration by: (1) adding parameterized test support to consolidate redundant spec tests, (2) enhancing unit test coverage for development-time feedback, and (3) adding an integration unit test layer that tests the compilation pipeline without execution.

## Context

The current test suite is comprehensive but has grown to a size that impacts development velocity:

| Test Type | Count | Purpose |
|-----------|-------|---------|
| Unit tests | 646 | Fast, focused component tests |
| Spec tests | 1,083 | Full integration tests with spec traceability |
| UI tests | 39 | Compiler diagnostic behavior |

### Problems Identified

1. **Spec Test Redundancy**: Analysis shows ~60-70% of spec tests could be consolidated:
   - `integers.toml`: 95 cases with 8 nearly identical patterns per type (8 types × N operations)
   - `arithmetic.toml`: 67 cases, 5+ tests covering the same spec paragraph
   - `let.toml`: 54 cases with excessive shadowing/naming variants
   - `functions.toml`: 27 inout tests that are structural duplicates

2. **Development Workflow Gap**: Developers must run the full spec suite to verify changes, when faster unit tests could catch most issues.

3. **Unit Test Coverage Gaps**: Some areas have limited unit tests:
   - RIR generation (13 tests)
   - CFG construction (21 tests)
   - No "integration unit tests" that test parse→codegen without execution

### Current Test Overlap

Many language features are tested at multiple levels:
- **Lexer**: 18 unit tests + ~100 spec tests for tokens
- **Parser**: 38 unit tests + extensive spec coverage
- **Type system**: 143 unit tests + 281 spec type cases
- **Codegen**: 200 unit tests + arithmetic/control flow spec tests

## Decision

### 1. Add Parameterized Test Support (Single-Process)

The key insight: instead of spawning N processes for N parameter combinations, generate a **single program** that tests all variants internally. This eliminates process overhead which dominates spec test time.

#### Test Format

```toml
[[case]]
name = "integer_return"
spec = ["3.1:1", "3.1:2", "3.1:3", "3.1:4", "3.1:8", "3.1:9", "3.1:10", "3.1:11"]
parameters = [
    { type = "i8", value = "42", expected = "42" },
    { type = "i16", value = "100", expected = "100" },
    { type = "i32", value = "42", expected = "42" },
    { type = "i64", value = "42", expected = "42" },
    { type = "u8", value = "42", expected = "42" },
    { type = "u16", value = "100", expected = "100" },
    { type = "u32", value = "42", expected = "42" },
    { type = "u64", value = "42", expected = "42" },
]
test_fn = """
fn test_${type}() -> ${type} { ${value} }
"""
expected_fn = "${expected}"  # Expected return value for each test_fn
```

#### Generated Program

The test runner generates a single program with all test functions and a harness:

```rust
// Generated from parameterized test
fn test_i8() -> i8 { 42 }
fn test_i16() -> i16 { 100 }
fn test_i32() -> i32 { 42 }
fn test_i64() -> i64 { 42 }
fn test_u8() -> u8 { 42 }
fn test_u16() -> u16 { 100 }
fn test_u32() -> u32 { 42 }
fn test_u64() -> u64 { 42 }

fn main() -> i32 {
    // Run all tests, return 0 on success or test index + 1 on first failure
    if test_i8() as i64 != 42 { return 1; }
    if test_i16() as i64 != 100 { return 2; }
    if test_i32() as i64 != 42 { return 3; }
    if test_i64() as i64 != 42 { return 4; }
    if test_u8() as i64 != 42 { return 5; }
    if test_u16() as i64 != 100 { return 6; }
    if test_u32() as i64 != 42 { return 7; }
    if test_u64() as i64 != 42 { return 8; }
    0  // All tests passed
}
```

#### Benefits

- **8 tests → 1 process**: Eliminates 7 process spawns per parameterized test
- **Compile once**: Single compilation for all variants
- **Clear failure reporting**: Exit code indicates which variant failed (0 = all pass, N = variant N failed)
- **Spec traceability preserved**: All spec paragraphs listed in the test definition

#### Template Syntax

- `${param_name}` for parameter interpolation
- Parameters are key-value maps with arbitrary keys
- `test_fn` defines the per-variant function template
- `expected_fn` defines the expected return value (as string, parsed per-type)

#### Failure Reporting

When a parameterized test fails:
```
FAILED: integers::integer_return (variant 3: type=i32, value=42)
  Expected: 42
  Actual: (exit code indicates variant 3 failed)
```

The test runner maps exit codes back to parameter sets for clear error messages.

### 2. Consolidate Spec Tests

Target consolidation in high-duplication files while maintaining 100% spec paragraph coverage:

| File | Current | Target | Strategy |
|------|---------|--------|----------|
| `integers.toml` | 95 | ~30 | Parameterize type variants |
| `arithmetic.toml` | 67 | ~25 | Parameterize operators, reduce redundant precedence tests |
| `let.toml` | 54 | ~20 | Remove excessive shadowing/naming variants |
| `functions.toml` (inout) | 27 | ~8 | Keep 1 representative per pattern (primitive, struct, array) |
| Other files | ~840 | ~350 | Review for similar patterns |

**Total target**: Reduce from ~1,083 to ~400-500 cases.

### 3. Add Integration Unit Tests

Create a new test layer in `rue-compiler` that tests the pipeline without execution:

```rust
// In rue-compiler/src/lib.rs
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_arithmetic_compiles() {
        let result = compile_to_air("fn main() -> i32 { 1 + 2 * 3 }");
        assert!(result.is_ok());
        // Can optionally verify AIR structure
    }

    #[test]
    fn test_type_mismatch_error() {
        let result = compile_to_air("fn main() -> i32 { true }");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("type mismatch"));
    }
}
```

Benefits:
- Fast: No file I/O, no process spawning, no execution
- Comprehensive: Tests full parse→sema→codegen pipeline
- Debuggable: Can inspect intermediate IRs in tests

### 4. Development Workflow

Update CLAUDE.md with recommended workflow:

```bash
# During development - fast feedback (unit tests only)
./buck2 test //crates/...             # Unit tests only (~2-5 seconds)

# Before committing - full verification
./test.sh                             # Full suite including spec tests

# Targeted spec tests
./buck2 run //crates/rue-spec:rue-spec -- "arithmetic"  # Run specific tests
```

Add a new script `./quick-test.sh` that runs only unit tests for faster iteration.

## Implementation Phases

- [ ] **Phase 1: Parameterized Test Support** - rue-9jdv.1
  - Add `parameters`, `test_fn`, `expected_fn` fields to `Case` struct in `rue-test-runner`
  - Implement template expansion with `${param}` syntax
  - Implement single-program generation with test harness
  - Implement exit-code-to-variant mapping for failure reporting
  - Add documentation for the new format

- [ ] **Phase 2: Consolidate Integer Tests** - rue-9jdv.2
  - Rewrite `integers.toml` using parameterized format
  - Verify 100% spec coverage maintained via traceability check
  - Target: 95 → ~30 cases

- [ ] **Phase 3: Consolidate Other Spec Tests** - rue-9jdv.3
  - Consolidate `arithmetic.toml`, `let.toml`, `functions.toml` (inout section)
  - Review and consolidate remaining test files
  - Target: Total ~1,083 → ~400-500 cases

- [ ] **Phase 4: Integration Unit Tests** - rue-9jdv.4
  - Add `compile_to_air()` and `compile_to_cfg()` test helpers to `rue-compiler`
  - Add integration unit tests covering major language features
  - Target: Add ~50-100 integration unit tests

- [ ] **Phase 5: Workflow Documentation** - rue-9jdv.5
  - Update CLAUDE.md with development workflow
  - Add `./quick-test.sh` script
  - Document when to use which test level

## Consequences

### Positive

- **Dramatically faster spec tests**: Single-process parameterized tests eliminate per-variant process overhead
  - Current: 8 type variants = 8 compile + 8 execute = 16 process spawns
  - New: 8 type variants = 1 compile + 1 execute = 2 process spawns
  - Estimated improvement: ~70-80% reduction in spec test time for parameterized tests
- **Faster development iteration**: Unit tests provide sub-second feedback
- **Maintained spec coverage**: Parameterization preserves traceability while reducing duplication
- **Better test organization**: Clear separation between development tests (fast) and verification tests (comprehensive)
- **Easier maintenance**: Parameterized tests mean one place to update when behavior changes

### Negative

- **Template syntax complexity**: Parameterized tests are slightly more complex to write
- **Migration effort**: Consolidating existing tests requires careful review
- **Potential coverage gaps**: Must verify traceability after each consolidation

### Neutral

- **Test count reduction**: Fewer tests is not inherently better or worse; coverage matters
- **Two test commands**: Developers need to remember when to use each

## Open Questions

1. **Template syntax**: Should we use `${param}` or `{param}` or `{{param}}`? (`${param}` matches shell conventions)

2. **Complex expressions**: Should parameterized exit codes support expressions like `${a} + ${b}`? (Probably not for v1 - YAGNI)

3. **Nested parameters**: Should we support arrays of arrays for testing combinations? (e.g., type × operator combinations)

## Future Work

- **Test performance metrics**: Add timing to test output to identify slow tests
- **Parallel spec tests**: Run spec tests in parallel for faster full-suite runs
- **Test coverage visualization**: Generate coverage reports showing which spec paragraphs are most/least tested

## References

- [ADR-0005: Preview Features](0005-preview-features.md) - Feature gating mechanism
- [CLAUDE.md](../../CLAUDE.md) - Development workflow documentation
- `crates/rue-test-runner/src/lib.rs` - Current test infrastructure
