# Rue Test Runner Scripts

This directory contains scripts for running and managing the Rue test runner framework.

## Scripts

### `run-tests.sh`

Interactive script for development and testing. Provides various commands for running tests, updating snapshots, and generating reports.

**Usage:**
```bash
./scripts/runner/run-tests.sh [command] [options]
```

**Commands:**
- `build` - Build rue compiler and test runner
- `test` - Run basic test suite  
- `report [file]` - Run tests with JSON report output
- `update-snapshots` - Update golden snapshots
- `filter <pattern>` - Run tests matching regex pattern
- `spec-compliance` - Run normative specification compliance tests
- `benchmark` - Benchmark test execution performance
- `help` - Show help message

**Examples:**
```bash
# Build tools and run all tests
./scripts/runner/run-tests.sh test

# Generate detailed JSON report
./scripts/runner/run-tests.sh report my-report.json

# Run only compilation tests
./scripts/runner/run-tests.sh filter 'compile.*'

# Update golden snapshots
./scripts/runner/run-tests.sh update-snapshots

# Check spec compliance across all tests
./scripts/runner/run-tests.sh spec-compliance
```

### `ci-tests.sh`

Continuous Integration script designed for automated testing environments. Provides comprehensive testing with structured output and proper exit codes.

**Usage:**
```bash
./scripts/runner/ci-tests.sh
```

**Features:**
- Builds tools in release mode for performance
- Runs comprehensive test suite across all test directories
- Validates test example syntax
- Checks for performance regressions
- Generates detailed JSON reports
- Provides structured logging with timestamps
- Returns appropriate exit codes for CI systems

**Environment Variables:**
- `RUST_LOG` - Set log level (default: `rue=info`)
- `RUST_BACKTRACE` - Enable backtraces (default: `1`)
- `NO_COLOR` - Disable colored output

## Test Directory Structure

The test runner expects the following directory structure:

```
tests/runner/                    # Test files
├── compile_pass_basic.rue      # Compilation success tests
├── compile_fail_type_error.rue # Compilation failure tests  
├── run_pass_arithmetic.rue     # Runtime success tests
├── run_fail_bounds_check.rue   # Runtime failure tests
├── snapshot_mir_simple.rue     # MIR snapshot tests
├── snapshot_asm_factorial.rue  # Assembly snapshot tests
└── snapshots/                  # Golden snapshots
    ├── simple.mir.snap
    └── factorial.asm.snap
```

## Test Directive Format

Tests use special comments to specify their behavior:

```rue
//!@ test-kind [attributes] [spec:references]
```

**Test Kinds:**
- `compile-pass` - Must compile successfully
- `compile-fail` - Must fail compilation
- `run-pass` - Must compile and run successfully  
- `run-fail` - Must compile but fail at runtime
- `snapshot-mir` - Compare MIR output against golden snapshot
- `snapshot-asm` - Compare assembly output against golden snapshot

**Attributes:**
- `stdout="text"` - Expected stdout content
- `stderr="text"` - Expected stderr content  
- `exit-code=N` - Expected exit code for run-fail tests
- `spec:name` - Reference to normative specification rule

**Examples:**
```rue
//!@ compile-pass spec:type-system.inference
//!@ run-pass stdout="42" spec:integers.arithmetic
//!@ run-fail exit-code=1 stderr="bounds" spec:runtime.bounds-checking
//!@ snapshot-mir spec:control-flow.return-values
```

## Normative Specification

The test runner validates tests against the normative specification in `spec/norms.toml`. This ensures implementation compliance with language requirements.

Specification categories include:
- `type-system` - Type checking and inference rules
- `runtime` - Runtime behavior requirements
- `control-flow` - Control flow semantics
- `functions` - Function call conventions
- `arrays` - Array operations and bounds checking
- `integers` - Integer arithmetic and operations
- `variables` - Variable scoping and mutability

## Integration with Build Systems

### Cargo
```bash
# Build and run test runner
cargo build --bin rue-runner
cargo run --bin rue-runner -- --help

# Run runner unit tests
cargo test --package rue-runner
```

### Buck2
```bash
# Build test runner
buck2 build //crates/rue-runner:rue-runner-bin

# Run unit tests
buck2 test //crates/rue-runner:test
```

## Performance Monitoring

The scripts include performance monitoring capabilities:

- **Benchmark mode** - Measures test execution time across multiple iterations
- **Regression detection** - Compares current performance against baseline
- **CI integration** - Automatically flags significant performance degradations

Performance baselines are stored in JSON format and can be updated as needed.

## Error Handling

Both scripts provide comprehensive error handling:

- **Build failures** - Clear error messages for compilation issues
- **Test failures** - Detailed failure reports with context
- **Missing dependencies** - Graceful handling of missing tools
- **File system errors** - Proper error reporting for I/O issues

## Customization

Scripts can be customized through environment variables and command-line options:

- Test paths can be modified to include additional directories
- Specification files can be pointed to different locations  
- Report formats and output paths are configurable
- Logging levels and formats can be adjusted

For more details on customization options, run the scripts with `--help` or examine the source code.