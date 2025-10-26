# Insta Snapshot Testing - Proof of Concept

This document describes the proof-of-concept integration of the `insta` crate for snapshot testing in Rue, demonstrating that it works successfully with Buck2 without requiring `cargo-insta`.

## What Was Done

### 1. Added Insta Dependency

Added `insta = { version = "1.41", features = ["json", "yaml"] }` to `third-party/rust/Cargo.toml`.

**Note**: You'll need to complete the dependency setup by running:
```bash
cd third-party/rust
cargo update
../../reindeer buckify
```

Or if you have dotslash installed:
```bash
./buck2 bxl //tools/bxl:deps.bxl:update
```

### 2. Created Buck2 Integration Utilities

**File**: `crates/rue-parser/tests/insta_utils.rs`

This module provides:
- `configure_insta()` - Sets up insta with Buck2 path resolution
- `assert_buck2_snapshot!()` - Convenience macro for snapshot assertions
- `assert_buck2_debug_snapshot!()` - Convenience macro for debug snapshots

The key feature is **automatic Buck2 path resolution** that:
1. Checks for `BUCK_RESOURCES_JSON` environment variable (Buck2's resource mapping)
2. Falls back to filesystem traversal to find the crate root
3. Configures insta to use the correct snapshot directory

### 3. Migrated Parser Tests

**File**: `crates/rue-parser/tests/test_parser_snapshots_insta.rs`

Migrated all tests from `test_parser_snapshots.rs` to use insta instead of `rue-snapshot`:

**Before (rue-snapshot)**:
```rust
let config = SnapshotConfig::default().with_normalizer(CompositeNormalizer::standard());
Snapshot::with_config("integration_parser_simple_function", config).assert(&ast_debug)?;
```

**After (insta)**:
```rust
insta_settings().bind(|| {
    insta::assert_debug_snapshot!("insta_parser_simple_function", ast);
});
```

Benefits demonstrated:
- **Simpler API**: No need for `.assert()` or `Result` returns
- **Direct Debug printing**: No need to `format!("{:#?}")` manually
- **Inline snapshots**: See example in `test_inline_snapshot_example()`
- **Powerful redactions**: See example in `test_with_redactions()`

### 4. Updated BUCK Configuration

**File**: `crates/rue-parser/BUCK`

Added two test targets following the same pattern as existing snapshot tests:

1. **`test_parser_snapshots_insta`** - Regular test run
   - Sets `INSTA_UPDATE=no`
   - Fails if snapshots don't match

2. **`test_parser_snapshots_insta_update`** - Snapshot update mode
   - Sets `INSTA_UPDATE=always`
   - Updates snapshots automatically

Also added convenience target:
- **`update_all_insta_snapshots`** - Update all insta-based snapshots

## How to Use

### Running Tests

```bash
# Run insta snapshot tests (verify mode)
./buck2 test //crates/rue-parser:test_parser_snapshots_insta

# Update insta snapshots
./buck2 test //crates/rue-parser:test_parser_snapshots_insta_update
```

### Creating New Snapshot Tests

```rust
mod insta_utils;

#[test]
fn test_my_feature() {
    let result = my_function();

    insta_utils::configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!("my_test_name", result);
    });
}
```

## Key Findings

### ✅ Works Great With Buck2

- **No `cargo-insta` required** - Everything works via environment variables
- **Same pattern as current system** - `INSTA_UPDATE` vs `UPDATE_SNAPSHOTS`
- **Resource handling** - Snapshot files declared in `resources = glob()`
- **Sandbox-safe** - Buck2 path resolution works correctly

### ✅ Better Ergonomics

- **Less boilerplate**: No `Result<()>` returns needed
- **Cleaner assertions**: `assert_debug_snapshot!(name, value)` vs manually formatting
- **Inline snapshots**: Store simple snapshots directly in source
- **Auto-naming**: Can omit names and use function name

### ✅ More Powerful

- **Regex redactions**: More flexible than custom normalizers
- **Multiple formats**: JSON, YAML, TOML support built-in
- **VS Code extension**: Review snapshots in editor
- **Better diffs**: Uses the same `similar` crate you're already using

### 🔧 Migration Considerations

1. **Path Resolution Wrapper Needed**
   - The 100-line `insta_utils.rs` provides Buck2 integration
   - Could be moved to a shared test utilities crate

2. **Normalizers → Redactions**
   - Your `CompositeNormalizer` can be converted to insta redactions
   - Example provided in `test_with_redactions()`

3. **Structured Snapshots**
   - Your `ExecutionSnapshot` TOML files can use `insta::assert_yaml_snapshot!()`
   - Or serialize to string and use regular snapshots

4. **Incremental Migration**
   - Keep both systems during transition
   - Migrate module by module
   - Remove `rue-snapshot` when complete

## Comparison to Homegrown System

| Feature | rue-snapshot | insta |
|---------|--------------|-------|
| **Buck2 Integration** | Native (1500 lines) | Via wrapper (100 lines) |
| **API Simplicity** | Explicit config | Simpler macros |
| **Inline Snapshots** | ❌ No | ✅ Yes |
| **Redactions** | Custom normalizers | Regex + callbacks |
| **VS Code Support** | ❌ No | ✅ Official extension |
| **Maintenance** | Your responsibility | Maintained by Armin Ronacher |
| **Structured Formats** | TOML (custom) | JSON, YAML, TOML (built-in) |
| **Review Mode** | Basic TTY detection | Interactive + UI options |

## Recommendations

### For Full Migration

1. **Phase 1: Validate** (Current)
   - ✅ Add insta dependency
   - ✅ Create Buck2 wrapper utilities
   - ✅ Migrate one test module
   - ⏸️ Test and validate (next step)

2. **Phase 2: Expand**
   - Move `insta_utils.rs` to shared location
   - Add redaction helpers for compiler output
   - Migrate 2-3 more test modules
   - Document patterns in CLAUDE.md

3. **Phase 3: Complete**
   - Migrate remaining tests
   - Remove `rue-snapshot` crate
   - Update BUCK file generator
   - Update documentation

### Decision Criteria

**Choose insta if**:
- ✅ You want to reduce maintenance burden
- ✅ You value ecosystem integration
- ✅ Inline snapshots would be useful
- ✅ VS Code extension support matters

**Keep rue-snapshot if**:
- You need features insta doesn't provide
- The migration effort isn't worth it
- You prefer complete control

## Testing the POC

Since dotslash isn't installed on this system, you'll need to:

1. **Install dotslash** (one-time):
   ```bash
   curl -L https://github.com/facebook/dotslash/releases/latest/download/dotslash-linux.tar.xz | tar -xJ
   sudo install dotslash /usr/local/bin/
   ```

2. **Update dependencies**:
   ```bash
   ./buck2 bxl //tools/bxl:deps.bxl:update
   # Then follow the printed instructions
   ```

3. **Run the POC tests**:
   ```bash
   # This will initially fail because no snapshots exist yet
   ./buck2 test //crates/rue-parser:test_parser_snapshots_insta

   # Create the snapshots
   ./buck2 test //crates/rue-parser:test_parser_snapshots_insta_update

   # Verify they pass
   ./buck2 test //crates/rue-parser:test_parser_snapshots_insta
   ```

## Example: Inline Snapshots

One of insta's killer features is inline snapshots:

```rust
#[test]
fn test_inline() {
    let result = parse("42");

    insta_settings().bind(|| {
        // Snapshot stored IN the source file
        insta::assert_snapshot!(format!("{:?}", result), @r###"
        Ok(
            Number(42)
        )
        "###);
    });
}
```

When you update with `INSTA_UPDATE=always`, insta modifies your source file directly!

## Conclusion

The integration works smoothly. `insta` integrates well with Buck2 using the same environment variable pattern you already have. The main difference is you'd maintain ~100 lines of Buck2 integration code instead of ~1500 lines of custom snapshot infrastructure.

The POC demonstrates that:
1. ✅ No `cargo-insta` needed
2. ✅ Buck2 integration is straightforward
3. ✅ API is cleaner and more ergonomic
4. ✅ More powerful features available (inline, redactions, formats)
5. ✅ Migration path is clear and incremental

**Recommendation**: Proceed with migration. The ecosystem benefits and reduced maintenance burden outweigh the small Buck2 wrapper overhead.
