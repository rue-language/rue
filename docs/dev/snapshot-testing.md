# Snapshot Testing Guide

This guide covers snapshot testing in the Rue project using Buck2.

## Quick Start

### Update Snapshots

```bash
# Update CLI test snapshots
./buck2 test //crates/rue:cli_tests_update

# Update parser snapshots
./buck2 test //crates/rue-parser:test_parser_snapshots_update

# Update corpus test snapshots
./buck2 test //crates/rue:snapshot_corpus_tests_update

# Update all parser-related snapshots
./buck2 test //crates/rue-parser:test_aggregate_types_update
./buck2 test //crates/rue-parser:test_comprehensive_parser_update
```

### Run Tests (Check Snapshots)

```bash
# Run regular tests (fail on mismatch)
./buck2 test //crates/rue:cli_tests
./buck2 test //crates/rue-parser:test_parser_snapshots
./buck2 test //crates/rue:snapshot_corpus_tests
```

### Find All Snapshot Targets

```bash
# List all snapshot test targets
./buck2 query "kind(rust_test, //...)" | grep -E "(snapshot|update)"

# List just the update targets
./buck2 query "kind(rust_test, //...)" | grep "_update$"
```

## Implementation Details

### How It Works

The snapshot testing system uses environment variables to control behavior:

- `UPDATE_SNAPSHOTS=1` - When set, tests update snapshots instead of failing
- `RUE_SNAPSHOT_DIR` - Override the default snapshot directory location

Buck2 doesn't support passing environment variables at runtime like `UPDATE_SNAPSHOTS=1 buck2 test`, so we create dedicated targets with the environment variable pre-configured.

### BUCK File Configuration

Use the `rust_snapshot_tests()` function to create both regular and update targets:

```starlark
load("//tools/rust:defs.bzl", "rust_snapshot_tests")

rust_snapshot_tests(
    name = "cli_tests",
    srcs = glob(["tests/**/*.rs"]),
    crate_root = "tests/cli_tests.rs",
    deps = [
        "//crates/rue-snapshot:rue-snapshot",
        # ... other deps
    ],
    snapshot_dir = "tests/snapshots/cli",
)
```

This creates two targets:

- `//crates/rue:cli_tests` - Regular test (fails on snapshot mismatch)
- `//crates/rue:cli_tests_update` - Update test (updates snapshots)

### Creating New Snapshot Tests

1. Use `rust_snapshot_tests()` in your BUCK file
2. Import the snapshot testing utilities in your test:

   ```rust
   use rue_snapshot::{ExecutionSnapshot, SnapshotTestBuilder};
   ```

3. Run the test with `_update` suffix to create initial snapshots

## Best Practices

1. **Commit snapshots** - Always commit snapshot files to version control
2. **Review changes** - Carefully review snapshot changes before committing
3. **Use descriptive names** - Name snapshots clearly to indicate what they test
4. **Normalize output** - Remove non-deterministic output (timestamps, paths) before snapshots
5. **Group related tests** - Use test suites to group related snapshot updates

## Troubleshooting

### "No snapshot found" Error

Run the test with the `_update` suffix to create the initial snapshot:

```bash
./buck2 test //crates/rue:test_name_update
```

### Snapshots Not Updating

Ensure you're using the `_update` target variant:

```bash
# Wrong - this just runs the test
./buck2 test //crates/rue:cli_tests

# Correct - this updates snapshots
./buck2 test //crates/rue:cli_tests_update
```

### Can't Find Update Target

Check that your BUCK file uses `rust_snapshot_tests()` instead of `rust_test()`.

The `rust_snapshot_tests()` macro automatically creates both:
- The regular test target (fails on mismatch)
- The `_update` variant (updates snapshots)

## Important Notes

### No Environment Variable Support

Buck2 does **not** support passing environment variables at runtime like:
```bash
# This DOES NOT work with Buck2:
UPDATE_SNAPSHOTS=1 ./buck2 test //crates/rue:cli_tests  # ❌ Won't work
```

Instead, use the dedicated `_update` targets that have `UPDATE_SNAPSHOTS=1` pre-configured.

### Finding Update Targets

Every snapshot test created with `rust_snapshot_tests()` automatically gets an `_update` variant:
- `//crates/rue:cli_tests` → `//crates/rue:cli_tests_update`
- `//crates/rue-parser:test_parser_snapshots` → `//crates/rue-parser:test_parser_snapshots_update`

## CI Integration

For CI pipelines:

```yaml
# Check snapshots are up to date
- run: ./buck2 test //crates/rue:cli_tests
- run: ./buck2 test //crates/rue-parser:test_parser_snapshots

# Run all tests
- run: ./buck2 test //crates/...
```

Never run snapshot updates in CI - they should only be updated locally and committed.