# Snapshot Testing in Rue

Rue uses the [insta](https://insta.rs) crate for snapshot testing, with Buck2 integration provided by the `rue-insta-utils` crate.

## Overview

Snapshot testing captures the output of a function/program and compares it against a previously saved "snapshot". This is particularly useful for:

- **Parser tests**: Verifying AST structure
- **Compiler tests**: Checking error messages and diagnostics
- **Corpus tests**: Validating program execution results
- **CLI tests**: Testing command-line behavior

## Quick Start

### Writing a Snapshot Test

```rust
use rue_insta_utils::configure_insta;

#[test]
fn test_parser() {
    let ast = parse("fn main() { 42 }");

    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!("test_parser_simple", ast);
    });
}
```

### Running Tests

```bash
# Run tests (verify mode - will fail if snapshots don't match)
./buck2 test //crates/rue-parser:test_aggregate_types

# Update snapshots (creates/updates .snap files)
./buck2 test //crates/rue-parser:test_aggregate_types_update

# Update all snapshots in a crate
INSTA_UPDATE=always ./buck2 test //crates/rue-parser:...
```

## API Reference

### Basic Snapshots

For simple text or debug output:

```rust
use rue_insta_utils::configure_insta;

#[test]
fn test_output() {
    let output = compile("fn main() { 42 }");

    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!("test_name", output);
    });
}
```

### Execution Snapshots

For program execution results (exit code, stdout, stderr):

```rust
use rue_insta_utils::execution::ExecutionSnapshot;

#[test]
fn test_execution() {
    let snapshot = ExecutionSnapshot {
        exit_code: 0,
        stdout: "Hello, world!\n".to_string(),
        stderr: String::new(),
        compilation_warnings: None,
        timeout: None,
    };

    let project_root = get_project_root();
    let snapshot_dir = project_root.join("tests/snapshots/corpus");

    rue_insta_utils::configure_insta(snapshot_dir.to_str().unwrap()).bind(|| {
        rue_insta_utils::assert_execution_snapshot!("test_hello", &snapshot);
    });
}
```

Execution snapshots are automatically serialized to TOML format for human readability.

### Inline Snapshots

Store simple snapshots directly in your test code:

```rust
#[test]
fn test_inline() {
    let result = parse("42");

    configure_insta("tests/snapshots").bind(|| {
        insta::assert_snapshot!(format!("{:?}", result), @"Number(42)");
    });
}
```

When you run with `INSTA_UPDATE=always`, insta will update the `@"..."` content automatically!

### Normalizations/Redactions

Remove non-deterministic output before snapshotting:

```rust
use rue_insta_utils::redactions::{normalize_timestamps, normalize_temp_paths};

#[test]
fn test_with_normalization() {
    let snapshot = get_execution_snapshot();

    let normalized = snapshot.normalize(|s| {
        let s = normalize_timestamps(s);
        normalize_temp_paths(&s)
    });

    assert_execution_snapshot!("test_name", &normalized);
}
```

Available normalizers:
- `normalize_timestamps()` - Replaces ISO 8601 timestamps with `[TIMESTAMP]`
- `normalize_temp_paths()` - Replaces `/tmp/...` with `[TEMP_PATH]`
- `normalize_temp_names()` - Replaces `t0`, `t1` with `[TEMP_VAR]`
- `normalize_addresses()` - Replaces `0x...` with `[ADDRESS]`
- `normalize_all()` - Applies all standard normalizations

You can also use insta's built-in redactions:

```rust
let mut settings = configure_insta("tests/snapshots");
settings.add_redaction(r"\b0x[0-9a-fA-F]+\b", "[ADDRESS]");
settings.add_redaction(r"/tmp/[^\s\"']+", "[TEMP_PATH]");

settings.bind(|| {
    insta::assert_snapshot!("test_name", output);
});
```

## Buck2 Integration

### BUCK File Configuration

For tests using snapshots, declare them in your BUCK file:

```python
rust_test(
    name = "test_parser_snapshots",
    srcs = glob(["tests/**/*.rs", "src/**/*.rs"]),
    crate_root = "tests/test_parser_snapshots.rs",
    edition = "2024",
    resources = glob([
        "tests/snapshots/**/*.snap",
    ]),
    env = {
        "INSTA_UPDATE": "no",
    },
    deps = [
        ":rue-parser",
        "//crates/rue-insta-utils:rue-insta-utils",
        "//third-party/rust:insta",
    ],
)

rust_test(
    name = "test_parser_snapshots_update",
    srcs = glob(["tests/**/*.rs", "src/**/*.rs"]),
    crate_root = "tests/test_parser_snapshots.rs",
    edition = "2024",
    resources = glob([
        "tests/snapshots/**/*.snap",
    ]),
    env = {
        "INSTA_UPDATE": "always",
    },
    deps = [
        ":rue-parser",
        "//crates/rue-insta-utils:rue-insta-utils",
        "//third-party/rust:insta",
    ],
)
```

This creates two targets:
- `test_parser_snapshots` - Runs tests, fails if snapshots don't match
- `test_parser_snapshots_update` - Updates snapshots

### Path Resolution

The `rue-insta-utils` crate handles Buck2's sandboxed build environment automatically:

1. Checks for `BUCK_RESOURCES_JSON` environment variable
2. Uses resource mapping to resolve snapshot paths
3. Falls back to filesystem traversal if needed

You don't need to worry about Buck2 paths - just use `configure_insta("tests/snapshots")` and it works.

## Environment Variables

Control snapshot behavior with environment variables:

- `INSTA_UPDATE=no` - Don't update snapshots (default, causes test to fail on mismatch)
- `INSTA_UPDATE=always` - Always update snapshots
- `INSTA_UPDATE=new` - Only create new snapshots, don't update existing ones
- `INSTA_UPDATE=unseen` - Update snapshots that haven't been reviewed

## Snapshot File Formats

### Debug Snapshots (`.snap`)

Plain text files with Rust's Debug representation:

```
tests/snapshots/test_name.snap:
---
source: crates/rue-parser/tests/test_parser_snapshots.rs
expression: ast
---
Ok(
    Program {
        functions: [
            Function {
                name: "main",
                return_type: I32,
                body: Number(42),
            },
        ],
    },
)
```

### Execution Snapshots (`.snap`)

TOML format for program execution results:

```toml
tests/snapshots/corpus/test_factorial.snap:
exit_code = 0
stdout = "120\n"
stderr = ""
```

## Best Practices

### 1. Use Descriptive Names

```rust
// Good
assert_debug_snapshot!("parser_function_with_params", ast);

// Bad
assert_debug_snapshot!("test1", ast);
```

### 2. Keep Snapshots Focused

Test one thing per snapshot. Split complex tests into multiple focused snapshots.

### 3. Review Snapshots Carefully

When updating snapshots:
1. Run the update command
2. Review the git diff to see what changed
3. Ensure changes are intentional
4. Commit snapshot files with your changes

### 4. Normalize Non-Deterministic Output

Always normalize timestamps, addresses, temp paths, etc.

```rust
// Before snapshotting, normalize:
let normalized = snapshot.normalize(normalize_all);
assert_execution_snapshot!("test", &normalized);
```

### 5. Use Inline Snapshots for Simple Cases

For small, stable outputs, inline snapshots keep everything in one place:

```rust
assert_snapshot!(format!("{:?}", result), @"Expected(Value)");
```

## Migrating from rue-snapshot

If you have tests using the old `rue-snapshot` crate:

**Before:**
```rust
use rue_snapshot::{Snapshot, SnapshotConfig};

fn assert_parser_snapshot(name: &str, source: &str) -> Result<()> {
    let ast = parse(source);
    let output = format!("{:#?}", ast);
    Snapshot::with_config(name, SnapshotConfig::default()).assert(&output)?;
    Ok(())
}
```

**After:**
```rust
use rue_insta_utils::configure_insta;

fn assert_parser_snapshot(name: &str, source: &str) {
    let ast = parse(source);
    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!(name, ast);
    });
}
```

Benefits:
- No need for `Result<()>` returns
- No manual `format!("{:#?}")`
- Direct AST snapshots
- Simpler API

## Troubleshooting

### Snapshots not found

Make sure:
1. Snapshot files are listed in `resources = glob([...])` in BUCK
2. Snapshot directory path is correct in `configure_insta(...)`
3. You've run the `_update` target at least once to create initial snapshots

### Path resolution issues

If snapshots can't be found in Buck2 builds:
1. Check that `resources.json` is being generated
2. Verify snapshot directory structure matches what's declared in BUCK
3. Use absolute paths as a fallback: `configure_insta(project_root.join("tests/snapshots").to_str().unwrap())`

### Snapshots differ on CI vs local

This usually means non-deterministic output. Add normalizations:
- Timestamps
- Temporary paths
- Memory addresses
- Generated IDs

## Resources

- [Insta documentation](https://insta.rs/docs/)
- [Insta GitHub repository](https://github.com/mitsuhiko/insta)
- `rue-insta-utils` source: `crates/rue-insta-utils/src/lib.rs`
- Example tests: `crates/rue-parser/tests/test_aggregate_types.rs`
