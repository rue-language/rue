---
id: 0015
title: Test Suite Optimization
status: accepted
tags: [process, testing, compiler]
feature-flag: null
created: 2025-12-26
accepted: 2025-12-26
implemented:
spec-sections: []
superseded-by:
---

# ADR-0015: Test Suite Optimization

## Status

Accepted

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

### 1. Parameterized Test Support

#### Phase 1: Multi-Process Expansion (Implemented)

The initial implementation uses a simple template expansion approach where each parameter set generates a separate test case that runs in its own process:

```toml
[[case]]
name = "{type}_return"
spec = ["3.1:1"]
params = [
  { type = "i8", value = "42", exit_code = 42, spec_extra = ["3.1:2"] },
  { type = "i16", value = "100", exit_code = 100, spec_extra = ["3.1:3"] },
  { type = "i32", value = "42", exit_code = 42, spec_extra = ["3.1:4"] },
  { type = "u8", value = "42", exit_code = 42, spec_extra = ["3.1:9"] },
]
source = "fn main() -> {type} { {value} }"
```

**Template syntax**: `{param_name}` placeholders are replaced with parameter values.

**Field overrides**: Parameters can override case fields like `exit_code`, `compile_fail`, `skip`, etc.

**Spec merging**: `spec_extra` in params is appended to the base `spec` array.

This approach:
- Reduces test file verbosity significantly (8 cases → 1 definition)
- Maintains spec traceability per variant via `spec_extra`
- Works with existing test infrastructure (no changes to test execution)
- Each variant still runs as a separate process

#### Future: Single-Process Execution (Phase 2+)

For maximum performance, a future enhancement could generate a single program that tests all variants internally:

```toml
[[case]]
name = "integer_return"
spec = ["3.1:1", "3.1:2", "3.1:3", "3.1:4", "3.1:8", "3.1:9", "3.1:10", "3.1:11"]
parameters = [
    { type = "i8", value = "42", expected = "42" },
    { type = "i16", value = "100", expected = "100" },
    # ...
]
test_fn = """
fn test_${type}() -> ${type} { ${value} }
"""
expected_fn = "${expected}"
```

This would generate a single program with all test functions and a harness, eliminating per-variant process overhead.

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

- [x] **Phase 1: Parameterized Test Support** - rue-9jdv.1
  - Added `ParamSet` struct and `params` field to `Case` in `rue-test-runner`
  - Implemented template expansion with `{param}` syntax
  - Implemented `expand_case()` and `expand_test_file()` functions
  - Added unit tests for expansion logic
  - Added example parameterized test to `integers.toml`

- [x] **Phase 2: Consolidate Integer Tests** - rue-9jdv.2
  - Rewrote `integers.toml` using parameterized format (95 → 41 case definitions)
  - Fixed traceability report to handle parameterized tests with `spec_extra`
  - Verified 100% spec coverage maintained via traceability check

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

- **Reduced test file verbosity**: 8 similar tests become 1 parameterized definition
- **Easier maintenance**: Change pattern once, affects all variations
- **Maintained spec coverage**: Parameterization preserves traceability via `spec_extra`
- **Better test organization**: Clear separation between development tests (fast) and verification tests (comprehensive)
- **Future optimization path**: Can evolve to single-process execution for further speedup

### Negative

- **Template syntax complexity**: Parameterized tests are slightly more complex to write
- **Migration effort**: Consolidating existing tests requires careful review
- **Potential coverage gaps**: Must verify traceability after each consolidation

### Neutral

- **Test count reduction**: Fewer tests is not inherently better or worse; coverage matters
- **Two test commands**: Developers need to remember when to use each

## Open Questions

1. ~~**Template syntax**: Should we use `${param}` or `{param}` or `{{param}}`?~~
   Resolved: Using `{param}` for simplicity in Phase 1.

2. **Single-process optimization**: Should Phase 2+ implement single-process test generation for performance? (Deferred - current approach may be sufficient)

3. **Nested parameters**: Should we support arrays of arrays for testing combinations? (e.g., type × operator combinations)

## Future Work

- **Single-process parameterized tests**: Generate one program testing all variants for maximum speed
- **Test performance metrics**: Add timing to test output to identify slow tests
- **Parallel spec tests**: Run spec tests in parallel for faster full-suite runs
- **Test coverage visualization**: Generate coverage reports showing which spec paragraphs are most/least tested

## References

- [ADR-0005: Preview Features](0005-preview-features.md) - Feature gating mechanism
- [CLAUDE.md](../../CLAUDE.md) - Development workflow documentation
- `crates/rue-test-runner/src/lib.rs` - Test infrastructure with parameterized support
- Similar concepts: pytest parametrize, JUnit @ParameterizedTest, Go table-driven tests
