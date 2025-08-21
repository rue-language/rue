# Snapshot Testing Guide

This guide covers snapshot testing in the Rue project using Buck2.

## Quick Start

### Update Snapshots

```bash
# Update all snapshots
./scripts/update-snapshots.sh

# Update specific test snapshots
./scripts/update-snapshots.sh cli       # CLI tests only
./scripts/update-snapshots.sh parser    # Parser tests only
./scripts/update-snapshots.sh corpus    # Corpus tests only

# Check snapshots without updating
./scripts/update-snapshots.sh --check

# List available snapshot targets
./scripts/update-snapshots.sh --list
```

### Direct Buck2 Commands

```bash
# Update specific test snapshots
./buck2 test //crates/rue:cli_tests_update
./buck2 test //crates/rue-parser:test_parser_snapshots_update

# Run regular tests (fail on mismatch)
./buck2 test //crates/rue:cli_tests
./buck2 test //crates/rue-parser:test_parser_snapshots
```

### BXL Commands

```bash
# Update all snapshots project-wide
./buck2 bxl //tools/bxl:snapshots.bxl:update_all

# Check all snapshots
./buck2 bxl //tools/bxl:snapshots.bxl:check_all

# List all snapshot targets
./buck2 bxl //tools/bxl:snapshots.bxl:list_targets
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

### BXL Script Not Finding Targets

The BXL script looks for targets with specific naming patterns:

- Ending with `_update`
- Containing `update_snapshot`
- Containing `snapshot` in the name

Ensure your test names follow these conventions.

## Migration from Cargo

If migrating from Cargo where you used `UPDATE_SNAPSHOTS=1 cargo test`:

1. Update BUCK files to use `rust_snapshot_tests()`
2. Use `./scripts/update-snapshots.sh` instead of the cargo command
3. Snapshot files remain in the same location and format

## CI Integration

For CI pipelines:

```yaml
# Check snapshots are up to date
- run: ./buck2 bxl //tools/bxl:snapshots.bxl:check_all

# Or use specific tests
- run: ./buck2 test //crates/rue:cli_tests
```

Never run snapshot updates in CI - they should only be updated locally and committed.