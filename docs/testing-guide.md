# Rue Compiler Testing Guide

This guide describes the testing infrastructure, best practices, and strategies for testing the Rue compiler.

## Table of Contents

1. [Testing Infrastructure Overview](#testing-infrastructure-overview)
2. [Types of Tests](#types-of-tests)
3. [Writing Effective Tests](#writing-effective-tests)
4. [Testing Tools and Frameworks](#testing-tools-and-frameworks)
5. [Best Practices](#best-practices)
6. [Running Tests](#running-tests)
7. [Continuous Integration](#continuous-integration)

## Testing Infrastructure Overview

The Rue compiler uses a multi-layered testing approach:

```
┌─────────────────────────────────────────────┐
│           Specification Tests               │
│         (rue-runner, spec-linked)           │
├─────────────────────────────────────────────┤
│          Integration Tests                  │
│    (End-to-end compilation & execution)     │
├─────────────────────────────────────────────┤
│           Property-Based Tests              │
│        (Invariants & properties)            │
├─────────────────────────────────────────────┤
│            Snapshot Tests                   │
│      (AST, MIR, Assembly output)           │
├─────────────────────────────────────────────┤
│             Unit Tests                      │
│    (Individual functions & modules)         │
└─────────────────────────────────────────────┘
```

## Types of Tests

### 1. Specification-Linked Tests (`rue-runner`)

These tests directly validate compliance with the language specification.

**Location:** `tests/spec/`, `tests/fixtures/`

**Format:**
```rue
//!@ id T-SPEC-001
//!@ kind run-pass
//!@ spec §3.1.1 MAIN-REQUIRED
//!@ expect exit 0

fn main() -> i32 {
    42
}
```

**Test Types:**
- `compile-pass` - Must compile successfully
- `compile-fail` - Must fail compilation with specific errors
- `run-pass` - Must compile and run with expected output
- `run-fail` - Must compile but fail at runtime
- `snapshot-mir` - Validates MIR output
- `snapshot-asm` - Validates assembly output

### 2. Snapshot Tests

Capture and validate compiler output at various stages.

**Location:** `crates/*/tests/`

**Example:**
```rust
use rue_snapshot::Snapshot;

#[test]
fn test_parser_output() -> Result<()> {
    let ast = parse("let x = 42;");
    Snapshot::new("parser_let_statement")
        .assert(&format!("{:#?}", ast))?;
    Ok(())
}
```

**Benefits:**
- Easy to review changes in output
- Automatic updates with `UPDATE_SNAPSHOTS=1`
- Path and timestamp normalization
- Diff visualization on failures

### 3. Property-Based Tests

Verify invariants and properties that must hold for all inputs.

**Location:** `crates/*/tests/test_*_properties.rs`

**Example:**
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn parser_never_panics(input in ".*") {
        // Parser should handle any input gracefully
        let _ = parse(&input);
    }
    
    #[test]
    fn optimizer_preserves_semantics(program in program_strategy()) {
        let original = execute(&program);
        let optimized = optimize(&program);
        let result = execute(&optimized);
        prop_assert_eq!(original, result);
    }
}
```

**Common Properties to Test:**
- Parser robustness (no panics)
- Type safety (well-typed programs don't get stuck)
- Optimizer correctness (preserves semantics)
- Round-trip properties (parse → print → parse)

### 4. Integration Tests

End-to-end tests that compile and run complete programs.

**Location:** `tests/fixtures/corpus/`

**Categories:**
- `arithmetic/` - Arithmetic operations
- `functions/` - Function calls and recursion
- `control_flow/` - If/else, while loops
- `types/` - Type system features
- `errors/` - Error handling

### 5. Unit Tests

Test individual functions and modules in isolation.

**Location:** In-module `#[cfg(test)]` blocks

**Example:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_constant_folding() {
        let expr = BinaryOp::Add(Constant(2), Constant(3));
        let folded = fold_constants(expr);
        assert_eq!(folded, Constant(5));
    }
}
```

## Writing Effective Tests

### Test Naming Conventions

```rust
// Unit tests: test_<component>_<scenario>
test_lexer_handles_unicode()
test_type_checker_detects_mismatch()

// Integration tests: <category>_<feature>_<expected_outcome>
arithmetic_overflow_fails()
functions_recursion_succeeds()

// Property tests: <component>_<property>
parser_never_panics()
optimizer_preserves_semantics()
```

### Test Organization

```
tests/
├── runner/           # Spec-linked tests
│   ├── compile_pass/
│   ├── compile_fail/
│   ├── run_pass/
│   └── run_fail/
├── fixtures/        # Test programs
│   └── corpus/     # Categorized test cases
└── benchmarks/     # Performance tests
```

### What to Test

1. **Happy Path** - Normal, expected usage
2. **Edge Cases** - Boundary conditions, empty inputs
3. **Error Cases** - Invalid inputs, type errors
4. **Regression Tests** - Previously fixed bugs
5. **Specification Compliance** - Language spec requirements

## Testing Tools and Frameworks

### rue-snapshot

Enhanced snapshot testing with normalization and diff visualization.

```rust
use rue_snapshot::{Snapshot, SnapshotConfig, normalize::CompositeNormalizer};

let config = SnapshotConfig::default()
    .with_normalizer(CompositeNormalizer::standard());

Snapshot::with_config("test_name", config)
    .assert(&output)?;
```

### rue-test-utils

Common testing utilities for compilation and execution.

```rust
use rue_test_utils::{RueCompiler, normalize_output};

let compiler = RueCompiler::new()?;
let result = compiler.compile_and_run(source)?;
assert_eq!(result.exit_code, 0);
```

### rue-runner

Specification-linked test runner with golden snapshot support.

```bash
# Run all spec tests
rue-runner --test-paths tests --rue-binary target/debug/rue

# Update golden snapshots
rue-runner --test-paths tests --rue-binary target/debug/rue --update-snapshots

# Generate coverage report
rue-runner --test-paths tests --rue-binary target/debug/rue --report-file coverage.json
```

### proptest

Property-based testing for invariants and correctness.

```rust
proptest! {
    #[test]
    fn test_property(input in strategy()) {
        // Test that property holds
    }
}
```

## Best Practices

### 1. Test at the Right Level

- **Unit tests** for algorithmic correctness
- **Integration tests** for feature validation
- **Property tests** for invariants
- **Spec tests** for language compliance

### 2. Use Snapshot Tests Judiciously

✅ **Good for:**
- AST structure
- Error messages
- Generated code
- Debug output

❌ **Avoid for:**
- Simple boolean checks
- Numeric results
- Performance metrics

### 3. Make Tests Deterministic

- Avoid random number generators without seeds
- Normalize paths, timestamps, and addresses
- Sort unordered collections before comparison
- Use fixed test data when possible

### 4. Write Descriptive Test Names

```rust
// Bad
test_1()
test_parser()

// Good
test_parser_recovers_from_missing_semicolon()
test_type_checker_infers_loop_variable_type()
```

### 5. Test Error Messages

```rust
#[test]
fn test_undefined_variable_error() {
    let result = compile("fn main() { x }");
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    assert!(error.message.contains("undefined variable"));
    assert_eq!(error.code, E2001);
}
```

### 6. Group Related Tests

```rust
mod lexer_tests {
    mod unicode {
        #[test]
        fn test_unicode_identifiers() { ... }
        
        #[test]
        fn test_unicode_strings() { ... }
    }
    
    mod numbers {
        #[test]
        fn test_integer_literals() { ... }
        
        #[test]
        fn test_overflow_detection() { ... }
    }
}
```

### 7. Use Test Fixtures

```rust
fn sample_program(name: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{}.rue", name))
        .expect("Failed to load fixture")
}

#[test]
fn test_factorial() {
    let program = sample_program("factorial");
    // Test with known program
}
```

## Running Tests

### All Tests
```bash
# Cargo
cargo test

# Buck2
buck2 test //crates/...
```

### Specific Test Suites
```bash
# Unit tests only
cargo test --lib

# Integration tests only
cargo test --test '*'

# Parser tests
cargo test -p rue-parser

# Property tests
cargo test -p rue-parser --test test_parser_properties

# Spec tests
./scripts/runner/run-tests.sh test
```

### With Output
```bash
# Show test output
cargo test -- --nocapture

# Verbose mode
cargo test -- --test-threads=1 --nocapture

# With logging
RUST_LOG=debug cargo test
```

### Update Snapshots
```bash
# Update CLI test snapshots
./buck2 test //crates/rue:cli_tests_update

# Update parser snapshots
./buck2 test //crates/rue-parser:test_parser_snapshots_update

# Update corpus snapshots
./buck2 test //crates/rue:snapshot_corpus_tests_update

# Find all update targets
./buck2 query "kind(rust_test, //...)" | grep "_update$"
```

## Continuous Integration

### GitHub Actions Workflow

Tests run automatically on:
- Pull requests
- Pushes to main branch
- Nightly schedule

### Test Coverage

Monitor test coverage with:
```bash
# Generate coverage report
cargo tarpaulin --out Html

# Check spec coverage
rue-runner --test-paths tests --rue-binary target/debug/rue --report-file coverage.json
```

### Performance Testing

```bash
# Run benchmarks
cargo bench

# Compare with baseline
cargo bench -- --baseline main
```

## Adding New Tests

### 1. Identify Test Type

- **Bug fix?** → Add regression test
- **New feature?** → Add spec test + integration tests
- **Optimization?** → Add property test for correctness
- **Error handling?** → Add compile-fail/run-fail tests

### 2. Write the Test

```rust
// For a new operator
#[test]
fn test_modulo_operator() {
    // Unit test for parser
    let ast = parse("x % 5");
    assert!(matches!(ast, Expr::Binary(BinaryOp::Mod, _, _)));
    
    // Integration test
    let result = compile_and_run("fn main() { 17 % 5 }");
    assert_eq!(result.exit_code, 2);
    
    // Property test
    proptest! {
        fn modulo_properties(a: i32, b: i32) {
            if b != 0 {
                let result = a % b;
                prop_assert!(result.abs() < b.abs());
            }
        }
    }
}
```

### 3. Link to Specification

```rue
//!@ id T-MOD-001
//!@ kind run-pass
//!@ spec integers.modulo
//!@ expect exit 2

fn main() -> i32 {
    17 % 5  // Returns 2
}
```

### 4. Update Documentation

- Add test to relevant test plan
- Update coverage metrics
- Document any special setup required

## Troubleshooting

### Test Failures

1. **Check error message** - Is it a legitimate failure?
2. **Run with --nocapture** - See actual vs expected output
3. **Check for race conditions** - Use --test-threads=1
4. **Verify environment** - Correct Rust version, dependencies

### Flaky Tests

Common causes:
- Non-deterministic behavior (timestamps, random values)
- File system dependencies
- Network calls
- Race conditions

Solutions:
- Use fixed seeds for randomness
- Mock external dependencies
- Normalize variable output
- Add retries for inherently flaky operations

### Performance Issues

- Run tests in parallel: `cargo test`
- Run specific tests: `cargo test test_name`
- Skip slow tests: `cargo test --skip slow`
- Use test categories: `#[ignore]` for expensive tests

## Summary

The Rue compiler testing infrastructure provides comprehensive validation through:

1. **Specification tests** ensure language compliance
2. **Snapshot tests** track output changes
3. **Property tests** verify invariants
4. **Integration tests** validate end-to-end behavior
5. **Unit tests** ensure component correctness

Follow the best practices in this guide to write effective, maintainable tests that catch bugs early and document expected behavior.