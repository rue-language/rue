# CI/CD Workflows

This directory contains GitHub Actions workflows for continuous integration and testing of the Rue compiler.

**Note:** The Rue compiler currently only supports Linux x86-64, so all CI workflows run on `ubuntu-latest` runners.

## Workflows Overview

### 1. `ci.yml` - Main CI Pipeline
**Triggers:** Push to trunk/main, Pull requests
**Purpose:** Core build and test validation

- Builds with Cargo and Buck2
- Runs standard test suite
- Tests with stable, beta, and nightly Rust
- Security audit and dependency checks
- Basic integration tests

### 2. `comprehensive-tests.yml` - Full Test Suite
**Triggers:** Push to trunk/main, Pull requests, Nightly schedule
**Purpose:** Exhaustive testing of all components

- **Property-based tests** with extended iterations (1000+ cases)
- **Specification compliance tests** using rue-runner
- **Snapshot testing** with diff uploads
- **Code coverage** with tarpaulin and Codecov
- **Test matrix** with stable and beta Rust (Linux only)
- **Integration test suite** with example programs
- **Performance benchmarks** (trunk/main only)

### 3. `pr-tests.yml` - Pull Request Validation
**Triggers:** Pull request events
**Purpose:** Fast feedback for PRs

- Quick formatting and clippy checks
- Property tests with reduced iterations
- Snapshot change detection
- Spec test summary
- Automatic PR comments with results

### 4. `nightly.yml` - Nightly Exhaustive Testing
**Triggers:** Daily at 3 AM UTC, Manual dispatch
**Purpose:** Deep testing and regression detection

- **Exhaustive property testing** (10,000+ iterations)
- **Fuzz testing** (when configured)
- **Sanitizer tests** (address, leak, memory, thread)
- **Minimal dependency versions** testing
- **Performance regression testing** with hyperfine
- Automatic issue creation on failures

## Test Coverage Strategy

### Unit Tests
- Run on every push and PR
- Part of standard `cargo test`

### Integration Tests
- Example programs in `examples/`
- Spec-linked tests in `tests/spec/` and other test subdirectories
- Validated with rue-runner

### Property-Based Tests
- Parser: 100-10,000 iterations based on context
- Type checker: 50-5,000 iterations
- Optimizer: 50-5,000 iterations
- HIR: 50-5,000 iterations

### Snapshot Tests
- Automatic detection of changes
- Upload diffs as artifacts
- PR blocks on uncommitted changes

### Performance Tests
- Benchmarks on trunk/main pushes
- Nightly regression testing
- Hyperfine for compilation speed

## Artifacts

Each workflow produces various artifacts:

- **proptest-regressions** - Failing property test cases
- **spec-test-report** - JSON report of spec compliance
- **snapshot-diffs** - Changes to snapshot tests
- **coverage-report** - HTML and XML coverage reports
- **benchmark-results** - Performance measurements
- **nightly-report** - Comprehensive nightly test summary

## Environment Variables

- `CARGO_TERM_COLOR: always` - Colored output
- `RUST_BACKTRACE: 1` - Show backtraces on panic
- `PROPTEST_CASES: N` - Number of property test iterations
- `UPDATE_SNAPSHOTS: 1` - Update snapshot files (local only)

## Manual Workflow Triggers

Some workflows support manual dispatch:

```bash
# Trigger nightly tests manually
gh workflow run nightly.yml

# Trigger comprehensive tests
gh workflow run comprehensive-tests.yml
```

## Local Testing

To run the same tests locally:

```bash
# Property tests with custom iterations
PROPTEST_CASES=1000 cargo test -p rue-parser --test test_parser_properties

# Spec-linked tests
# Note: Spec validation is currently disabled in exec.rs due to spec reference format mismatch
# The runner now works with warnings for invalid spec references
cargo run -p rue-runner -- \
  --test-paths tests \
  --rue-binary target/debug/rue

# Update snapshots
UPDATE_SNAPSHOTS=1 cargo test

# Run with sanitizers
RUSTFLAGS="-Z sanitizer=address" cargo test -Z build-std
```

## Adding New Tests

1. **Unit tests** - Add to relevant crate's `src/` or `tests/`
2. **Integration tests** - Add to `tests/spec/` or other appropriate test subdirectory with spec directives
3. **Property tests** - Add to `tests/test_*_properties.rs`
4. **Snapshots** - Use `rue-snapshot` crate in tests
5. **Benchmarks** - Add to `benches/` directory

## Maintenance

### Updating Test Iterations
Adjust `PROPTEST_CASES` in workflows:
- PR: 50-100 (fast feedback)
- Main: 500-1000 (thorough)
- Nightly: 5000-10000 (exhaustive)

### Adding New Test Categories
1. Update relevant workflow file
2. Add job with appropriate triggers
3. Upload artifacts if needed
4. Update this README

### Monitoring
- Check Actions tab for run history
- Review nightly reports for trends
- Monitor Codecov for coverage changes
- Track performance in benchmark artifacts