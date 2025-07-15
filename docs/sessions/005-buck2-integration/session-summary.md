# Session 5: Buck2 Integration Test Unification

## Overview
Successfully unified Buck2 and Cargo test execution by implementing `CARGO_MANIFEST_DIR` environment variable solution to resolve project root directory detection issues. Tests are constrained to Linux x86-64 platform since Rue generates Linux ELF executables.

## Problem
The integration tests in `crates/rue/tests/integration_tests.rs` worked perfectly with Cargo but failed with Buck2 due to different working directory contexts. Buck2 couldn't locate the project root directory, causing all end-to-end tests to fail.

## Solution Discovery
Found inspiration from the SystemInit (SI) project's BUCK configuration that sets `CARGO_MANIFEST_DIR = "."` in the test environment. This allows Buck2 tests to use the same CARGO_MANIFEST_DIR detection logic as Cargo tests.

## Implementation Details

### Buck2 Configuration Changes
Updated `crates/rue/BUCK` to include environment variable:
```rust
rust_test(
    name = "test",
    srcs = glob(["tests/**/*.rs"]),
    crate_root = "tests/integration_tests.rs",
    edition = "2024",
    deps = [
        "//crates/rue-compiler:rue-compiler",
        "//crates/rue-codegen:rue-codegen",
    ],
    env = {
        "CARGO_MANIFEST_DIR": ".",
    },
)
```

### Path Resolution Logic Enhancement
Enhanced `get_project_root()` function in `integration_tests.rs` to handle both Cargo and Buck2 contexts:

- **Cargo case**: `CARGO_MANIFEST_DIR` points to `crates/rue`, navigate up two levels
- **Buck2 case**: `CARGO_MANIFEST_DIR` is ".", resolve to current working directory (already at project root)
- **Fallback**: Search upward for directory containing both `Cargo.toml` and `crates/`

### Code Cleanup
- Removed redundant `buck2_simple_tests.rs` file that was created as a workaround
- Unified all end-to-end tests under single `integration_tests.rs` file
- Both build systems now run identical comprehensive test suites
- Removed cross-platform CI workflow since tests only work on Linux x86-64

## Test Results
All tests now pass in both environments:

**Buck2**: 3 tests (test_simple_program, test_factorial_program, test_all_samples_compile)
**Cargo**: 3 tests (same comprehensive end-to-end tests)

## Impact
- Eliminated build system fragmentation in test execution
- Both Buck2 and Cargo now run identical comprehensive end-to-end tests
- Improved developer experience with consistent test behavior
- Simplified maintenance by removing duplicate test files
- Clarified platform support constraints (Linux x86-64 only)

## Technical Notes
- The key insight was using `CARGO_MANIFEST_DIR` as a unified environment variable
- Buck2 sets this to "." while Cargo sets it to the actual manifest directory path
- Path resolution logic handles both cases gracefully
- Solution maintains backward compatibility with existing Cargo workflows
- Platform constraint: Tests only work on Linux x86-64 since Rue generates Linux ELF executables
- Removed cross-platform CI testing to avoid failures on unsupported platforms