# Rue Test Runner

A unified spec-linked test framework for the Rue compiler that provides comprehensive testing capabilities including compilation validation, runtime testing, and golden snapshot comparisons.

## Features

- **Inline test directives** - Test configuration directly in `.rue` source files via comments
- **Specification compliance** - Links tests to normative language specification rules
- **Multiple test types** - compile-pass/fail, run-pass/fail, snapshot-mir/asm
- **Golden snapshots** - Automatic management of expected outputs with `--update`
- **Coverage reporting** - Track which specification rules have test coverage
- **CI/CD ready** - JSON reports and proper exit codes for automation

## Quick Start

### Running Tests

```bash
# Run all tests
rue-runner --test-paths tests --rue-binary target/debug/rue --spec-file spec/norms.toml

# Update golden snapshots
rue-runner --test-paths tests --rue-binary target/debug/rue --update-snapshots

# Filter tests by pattern (matches file paths)
rue-runner --test-paths tests --rue-binary target/debug/rue --filter "run_pass"

# Generate test report
rue-runner --test-paths tests --rue-binary target/debug/rue --report-file report.json

# Continue despite failures
rue-runner --test-paths tests --rue-binary target/debug/rue --ignore-failures

# Verbose output
rue-runner --test-paths tests --rue-binary target/debug/rue --verbose
```

### Using with Cargo

```bash
# Build both compiler and test runner
cargo build -p rue -p rue-runner

# Run tests
cargo run -p rue-runner -- --test-paths tests --rue-binary target/debug/rue
```

### Using with Buck2

```bash
# Build and run tests
buck2 run //crates/rue-runner:spec_tests

# Or build separately
buck2 build //crates/rue:rue //crates/rue-runner:rue-runner
```

## Test File Format

Tests are `.rue` files with directive comments at the beginning. Directives use the `//!@` prefix.

### Directive Syntax

```rue
//!@ key value...
```

Multiple directives can be specified. Values are space-separated and extend to end of line.

### Required Directives

- `id <TEST_ID>` - Unique test identifier
- `kind <TYPE>` - Test type (see below)
- `spec <RULE_ID>` - Reference to specification rule (can be repeated)

### Optional Directives

- `expect exit <CODE>` - Expected exit code for run tests (default: 0 for run-pass)
- `expect stdout "<LINE>"` - Expected stdout lines (exact match, order matters)
- `expect stderr "<SUBSTR>"` - Required stderr substrings (any order)
- `flags <ARGS>` - Additional compiler flags

### Test Types

| Kind | Description | Validation |
|------|-------------|------------|
| `compile-pass` | Must compile successfully | Exit code 0 |
| `compile-fail` | Must fail compilation | Non-zero exit, optional stderr patterns |
| `run-pass` | Must compile and run successfully | Exit code, optional stdout |
| `run-fail` | Must compile but fail at runtime | Specific exit code required |
| `snapshot-mir` | Compare MIR output | Diff against `.rue.mir.golden` |
| `snapshot-asm` | Compare assembly output | Diff against `.rue.asm.golden` |

## Examples

### Compile-Pass Test

```rue
//!@ id T-COMP-BASIC-001
//!@ spec §3.1.1 MAIN-REQUIRED
//!@ kind compile-pass

fn main() -> i32 {
    0
}
```

### Compile-Fail Test

```rue
//!@ id T-TYP-ASSIGN-STRICT-001
//!@ spec §4.3.4.1 ASSIGN-STRICT
//!@ kind compile-fail
//!@ expect stderr "type mismatch" "expected i32" "found bool"

fn main() -> i32 {
    let x: i32 = true;  // Type error
    x
}
```

### Run-Pass Test

```rue
//!@ id T-RT-ARITH-001
//!@ spec §5.2.3 ARITH-WRAP-INT
//!@ kind run-pass
//!@ expect exit 0
//!@ expect stdout "42"

fn main() -> i32 {
    println(42);
    0
}
```

### Run-Fail Test

```rue
//!@ id T-RT-BOUNDS-001
//!@ spec §5.2.11.3 BOUNDS-EXIT-252
//!@ kind run-fail
//!@ expect exit 252

fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    a[3]  // Out of bounds
}
```

### Snapshot Test

```rue
//!@ id T-MIR-CONSTPROP-001
//!@ spec §5.2.3 ARITH-WRAP-INT
//!@ kind snapshot-mir

fn main() -> i32 {
    1 + 2  // Should be constant-propagated
}
```

## Specification Rules

Tests reference normative rules defined in `spec/norms.toml`:

```toml
[[rule]]
id = "§5.2.11.3 BOUNDS-EXIT-252"
level = "MUST"
text = "Out-of-bounds array access terminates with exit code 252."
```

Rule levels:
- `MUST` - Required for conformance
- `SHOULD` - Recommended for quality
- `MAY` - Optional enhancements

## CLI Options

| Option | Description | Default |
|--------|-------------|---------|
| `--test-paths <PATHS>` | Paths to search for test files | tests examples |
| `--rue-binary <PATH>` | Path to rue compiler binary | rue |
| `--spec-file <PATH>` | Path to specification file | spec/norms.toml |
| `--snapshot-dir <DIR>` | Directory for golden snapshots | tests/snapshots |
| `--report-file <FILE>` | Output file for JSON report | None |
| `--update-snapshots` | Update golden snapshots | false |
| `--ignore-failures` | Continue despite failures | false |
| `--filter <PATTERN>` | Filter tests by regex (file paths) | All tests |
| `--verbose` | Verbose output | false |

## Output Normalization

Snapshots are normalized before comparison:

1. **Paths** - Strip absolute paths, normalize separators
2. **Line endings** - Convert to `\n`
3. **Addresses** - Remove memory addresses and pointers
4. **Timestamps** - Remove date/time stamps
5. **Temp names** - Stabilize auto-generated identifiers (t0, t1, ...)

## JSON Report Format

```json
{
  "test_results": [
    {
      "id": "T-RT-BOUNDS-001",
      "kind": "run-fail",
      "status": "passed",
      "spec_rules": ["§5.2.11.3 BOUNDS-EXIT-252"],
      "duration_ms": 42
    }
  ],
  "spec_coverage": {
    "covered": ["§5.2.11.3 BOUNDS-EXIT-252"],
    "uncovered": ["§4.3.1 VAR-INIT"],
    "coverage_percent": 85.7
  },
  "summary": {
    "total": 100,
    "passed": 98,
    "failed": 2,
    "duration_ms": 1234
  }
}
```

## CI/CD Integration

### GitHub Actions

```yaml
- name: Run Rue Tests
  run: |
    cargo build -p rue -p rue-runner
    target/debug/rue-runner \
      --test-paths tests \
      --rue-binary target/debug/rue \
      --spec-file spec/norms.toml \
      --report-file test-report.json
```

## Directory Structure

```
rue/
├── spec/
│   └── norms.toml         # Normative specification rules
├── tests/
│   ├── runtime/
│   │   └── bounds.rue     # Runtime test examples
│   ├── type/
│   │   └── assign.rue     # Type checking tests
│   └── mir/
│       ├── constprop.rue  # MIR snapshot test
│       └── constprop.rue.mir.golden  # Expected output
└── crates/rue-runner/
    └── src/
        ├── main.rs        # Entry point
        ├── cli.rs         # Command-line interface
        ├── discover.rs    # Test file discovery
        ├── directives.rs  # Directive parsing
        ├── spec.rs        # Specification handling
        ├── exec.rs        # Test execution
        ├── snapshot.rs    # Golden file management
        └── report.rs      # Report generation
```

## Development

### Adding New Test Types

1. Update `TestKind` enum in `directives.rs`
2. Add execution logic in `exec.rs`
3. Update directive parser if new attributes needed
4. Document in this README

### Custom Normalization

Add normalizers in `snapshot.rs`:

```rust
fn normalize_custom(content: &str) -> String {
    // Your normalization logic
}
```

### Debugging Tests

```bash
# Run single test with verbose output
RUST_LOG=debug rue-runner \
  --test-paths tests \
  --rue-binary target/debug/rue \
  --filter "T-RT-BOUNDS-001" \
  --verbose

# Keep intermediate files
RUE_KEEP_TEMPS=1 rue-runner --test-paths tests --rue-binary target/debug/rue
```

## License

Same as Rue compiler - see repository LICENSE file.