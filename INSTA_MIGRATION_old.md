# Insta Migration - Full Plan and Progress

This document tracks the complete migration from `rue-snapshot` to `insta`.

## Executive Summary

**Status**: In Progress
**Started**: Session 011CUWK67pp72Kv5dLfLBpwT
**Completion**: ~60% complete

The migration from the homegrown `rue-snapshot` crate to the industry-standard `insta` crate is well underway. This migration will:

- **Reduce maintenance burden**: From ~1,500 lines of custom code to ~300 lines of Buck2 integration
- **Improve developer experience**: Simpler API, inline snapshots, better tooling
- **Enhance ecosystem integration**: VS Code extension, active maintenance, widely adopted

## What's Been Completed

### ✅ 1. Infrastructure (100% Complete)

**`crates/rue-insta-utils/` - Shared utilities crate**
- `src/lib.rs` - Buck2 path resolution and configuration (150 lines)
- `src/execution.rs` - ExecutionSnapshot support with TOML serialization (150 lines)
- `src/redactions.rs` - Standard normalization functions (100 lines)
- `BUCK` - Build configuration

**Key Features**:
- `configure_insta()` - Handles Buck2 resource mapping and path resolution
- `assert_execution_snapshot!()` - Macro for execution snapshots with TOML format
- `normalize_*()` functions - Compatible with old `rue-snapshot::normalize` module
- Drop-in replacement macros that work with existing test patterns

### ✅ 2. Dependency Setup (100% Complete)

- Added `insta = { version = "1.41", features = ["json", "yaml"] }` to `third-party/rust/Cargo.toml`
- All dependencies declared in `rue-insta-utils/BUCK`

**Note**: Requires running `./buck2 bxl //tools/bxl:deps.bxl:update` to regenerate Buck2 build files

### ✅ 3. Parser Test Migration (75% Complete)

**Fully Migrated**:
- ✅ `test_aggregate_types.rs` - All struct/enum/array/tuple tests (546 lines)
- ✅ `test_comprehensive_parser.rs` - Complete AST coverage (686 lines)
- ✅ `test_parser_snapshots_insta.rs` - POC with inline snapshot examples (199 lines)

**Pattern Used**:
```rust
// OLD (rue-snapshot):
use rue_snapshot::{Snapshot, SnapshotConfig};
fn assert_parser_snapshot(name: &str, source: &str) -> Result<()> {
    let result = parse_with_diagnostics(source, "test.rue");
    let output = format!("{:#?}", result);
    Snapshot::with_config(name, SnapshotConfig::default()).assert(&output)?;
    Ok(())
}

// NEW (insta):
use rue_insta_utils::configure_insta;
fn assert_parser_snapshot(name: &str, source: &str) {
    let result = parse_with_diagnostics(source, "test.rue");
    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!(name, result);
    });
}
```

**Benefits Realized**:
- No more `Result<()>` returns - cleaner test signatures
- No manual `format!("{:#?}")` - insta handles it
- Direct snapshot of AST instead of strings
- Simpler, more readable code

**Still TODO**:
- `test_parser_snapshots.rs` - Needs cleanup (currently has sed artifacts)
- `test_diagnostics.rs` - Small file, easy migration
- `test_error_recovery.rs` - Small file, easy migration
- `test_parser_properties.rs` - Property tests, may not need snapshots

### ✅ 4. Corpus Test Migration (90% Complete)

**File**: `crates/rue/tests/snapshot_corpus_tests_new.rs` (complete new version)

**Changes**:
- Uses `rue_insta_utils::execution::ExecutionSnapshot`
- Uses `assert_execution_snapshot!()` macro
- Snapshots automatically serialized to TOML format
- All corpus tests migrated

**Pattern Used**:
```rust
// OLD:
SnapshotTestBuilder::new(&test_name)
    .with_snapshot_dir(project_root.join("tests/snapshots/corpus"))
    .with_format(SnapshotFormat::Toml)
    .test_execution(&result)
    .expect("Snapshot test failed");

// NEW:
assert_corpus_snapshot(&test_name, &result);
// where:
fn assert_corpus_snapshot(name: &str, snapshot: &ExecutionSnapshot) {
    let project_root = get_project_root();
    let snapshot_dir = project_root.join("tests/snapshots/corpus");
    rue_insta_utils::configure_insta(snapshot_dir.to_str().unwrap()).bind(|| {
        rue_insta_utils::assert_execution_snapshot!(name, snapshot);
    });
}
```

**TODO**: Rename to replace old file, update BUCK

### 🔄 5. CLI Test Migration (Not Started)

**File**: `crates/rue/tests/cli_tests.rs`

**Needs**:
- Similar pattern to corpus tests
- Uses `ExecutionSnapshot` with normalization
- Should use `rue_insta_utils::redactions::normalize_*` functions

**Example Migration Needed**:
```rust
// OLD:
use rue_snapshot::{
    normalize::{CompositeNormalizer, FnNormalizer, normalize_timestamps, normalize_temp_names},
};

fn normalize_execution_snapshot(snapshot: &ExecutionSnapshot) -> ExecutionSnapshot {
    let normalizer = CompositeNormalizer::new()
        .with(FnNormalizer::new(normalize_timestamps))
        .with(FnNormalizer::new(normalize_temp_names));
    ExecutionSnapshot {
        exit_code: snapshot.exit_code,
        stdout: normalizer.normalize(&snapshot.stdout),
        stderr: normalizer.normalize(&snapshot.stderr),
        // ...
    }
}

// NEW:
use rue_insta_utils::redactions::{normalize_timestamps, normalize_temp_names, normalize_all};

fn normalize_execution_snapshot(snapshot: &ExecutionSnapshot) -> ExecutionSnapshot {
    snapshot.clone().normalize(|s| {
        let s = normalize_timestamps(s);
        normalize_temp_names(&s)
    })
}

// Or use the ExecutionSnapshot::normalize() method added in rue-insta-utils
```

## What Remains

### 🔲 6. BUCK File Updates (Critical)

All BUCK files need updates to:
1. Replace `rue-snapshot` dependency with `rue-insta-utils`
2. Change `INSTA_UPDATE` environment variable instead of `UPDATE_SNAPSHOTS`
3. Update test target patterns

**Files to Update**:
- `crates/rue-parser/BUCK` - Parser tests
- `crates/rue/BUCK` - Corpus and CLI tests
- Any other crates using snapshot testing

**Pattern**:
```python
# OLD:
rust_snapshot_tests(
    name = "test_parser_snapshots",
    deps = [
        "//crates/rue-snapshot:rue-snapshot",
    ],
    # Uses UPDATE_SNAPSHOTS env var
)

# NEW:
rust_test(
    name = "test_parser_snapshots",
    env = {"INSTA_UPDATE": "no"},
    deps = [
        "//crates/rue-insta-utils:rue-insta-utils",
        "//third-party/rust:insta",
    ],
)

rust_test(
    name = "test_parser_snapshots_update",
    env = {"INSTA_UPDATE": "always"},
    deps = [
        "//crates/rue-insta-utils:rue-insta-utils",
        "//third-party/rust:insta",
    ],
)
```

### 🔲 7. Remove `rue-snapshot` Crate

**After all tests are migrated and passing**:
1. Delete `crates/rue-snapshot/` directory
2. Remove from root BUCK/workspace configuration
3. Remove any references in documentation

### 🔲 8. Documentation Updates

**Files to Update**:
- `CLAUDE.md` - Update testing commands and patterns
- `docs/dev/snapshot-testing.md` - Complete rewrite for insta
- `docs/testing-guide.md` - Update examples
- `README.md` - Update if it mentions testing

**New Content Needed**:
```markdown
## Snapshot Testing with Insta

Rue uses the `insta` crate for snapshot testing, with Buck2 integration provided by `rue-insta-utils`.

### Running Snapshot Tests

```bash
# Run tests (verify mode)
./buck2 test //crates/rue-parser:test_parser_snapshots

# Update snapshots
./buck2 test //crates/rue-parser:test_parser_snapshots_update

# Environment variable control
INSTA_UPDATE=always ./buck2 test //crates/...
```

### Writing Snapshot Tests

```rust
use rue_insta_utils::configure_insta;

#[test]
fn test_feature() {
    let result = my_function();
    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!("test_name", result);
    });
}
```

### Inline Snapshots

```rust
#[test]
fn test_simple() {
    let output = parse("42");
    configure_insta("tests/snapshots").bind(|| {
        insta::assert_snapshot!(format!("{:?}", output), @"Number(42)");
    });
}
```

When you run with `INSTA_UPDATE=always`, insta updates the `@"..."` content in your source file automatically!
```

### 🔲 9. Remove POC Files

**After migration is complete**:
- Delete `INSTA_POC.md` (temporary analysis document)
- Delete `test_parser_snapshots_insta.rs` (was POC, now redundant)
- Delete `insta_utils.rs` from parser tests (functionality moved to `rue-insta-utils`)

### 🔲 10. Final Testing

**Before declaring complete**:
1. Run all tests: `./buck2 test //crates/...`
2. Update all snapshots: Run all `*_update` targets
3. Verify tests pass: `./buck2 test //crates/...` again
4. Check no references to `rue-snapshot` remain: `git grep rue-snapshot`
5. Ensure documentation is accurate
6. Test on CI if available

## Migration Commands

### For New Contributors

Once this migration is complete, here are the commands developers will use:

```bash
# Run specific snapshot tests
./buck2 test //crates/rue-parser:test_aggregate_types

# Update specific snapshots
./buck2 test //crates/rue-parser:test_aggregate_types_update

# Run all tests
./buck2 test //crates/...

# Update all snapshots at once
INSTA_UPDATE=always ./buck2 test //crates/...

# Review snapshots interactively (if cargo-insta is installed)
# NOTE: cargo-insta is OPTIONAL - not required for Buck2 workflow
cargo insta review
```

## Benefits Summary

### Before (rue-snapshot)
- ~1,500 lines of custom snapshot infrastructure
- Manual path resolution logic for Buck2
- Custom normalizers with complex API
- No inline snapshots
- No editor integration
- Maintenance burden on the Rue team

### After (insta)
- ~300 lines of Buck2 integration code
- Industry-standard library (used by Ruff, rustpython, tree-sitter, etc.)
- Simple regex-based redactions
- Inline snapshots supported
- VS Code extension available
- Maintained by Armin Ronacher (Flask, Jinja2, Ruff author)

### Code Reduction
```
rue-snapshot crate:     ~1,500 lines
rue-insta-utils crate:    ~400 lines
Net reduction:          ~1,100 lines

Plus deletion of:
- Buck2 macro complexity (rust_snapshot_tests)
- BXL snapshot management scripts
- Update scripts and helpers
```

### API Improvement
```rust
// Before: 5 lines
let output = format!("{:#?}", ast);
Snapshot::with_config("test_name", SnapshotConfig::default())
    .with_normalizer(CompositeNormalizer::standard())
    .assert(&output)?;
Ok(())

// After: 3 lines
configure_insta("tests/snapshots").bind(|| {
    insta::assert_debug_snapshot!("test_name", ast);
});
```

## Testing the Migration

### Prerequisites
```bash
# Install dotslash (one-time)
curl -L https://github.com/facebook/dotslash/releases/download/v0.4.2/dotslash-linux.v0.4.2.tar.gz | tar -xz
sudo install dotslash /usr/local/bin/

# Update Rust dependencies
cd /home/user/rue
./buck2 bxl //tools/bxl:deps.bxl:update
# Follow printed instructions to run cargo update and reindeer buckify
```

### Test Migrated Components
```bash
# Test the utilities crate
./buck2 test //crates/rue-insta-utils:test

# Test migrated parser tests
./buck2 test //crates/rue-parser:test_aggregate_types

# Create snapshots for new tests
./buck2 test //crates/rue-parser:test_aggregate_types_update

# Verify they pass
./buck2 test //crates/rue-parser:test_aggregate_types
```

## Next Steps (Priority Order)

1. **Update BUCK files** for migrated tests (enables testing)
2. **Migrate CLI tests** (similar pattern to corpus tests)
3. **Complete parser test cleanup** (fix `test_parser_snapshots.rs`)
4. **Replace corpus test file** (rename `*_new.rs` to replace old)
5. **Run full test suite** to verify everything works
6. **Update documentation** (CLAUDE.md, testing guides)
7. **Remove rue-snapshot crate** and POC files
8. **Final testing** and cleanup

## Files Modified

### Created
- `crates/rue-insta-utils/src/lib.rs`
- `crates/rue-insta-utils/src/execution.rs`
- `crates/rue-insta-utils/src/redactions.rs`
- `crates/rue-insta-utils/BUCK`
- `crates/rue/tests/snapshot_corpus_tests_new.rs`
- `INSTA_MIGRATION.md` (this file)

### Modified
- `third-party/rust/Cargo.toml` - Added insta dependency
- `crates/rue-parser/tests/test_aggregate_types.rs` - Migrated to insta
- `crates/rue-parser/tests/test_comprehensive_parser.rs` - Migrated to insta
- `crates/rue-parser/tests/test_parser_snapshots.rs` - Partially migrated (needs cleanup)
- `crates/rue-parser/tests/test_parser_snapshots_insta.rs` - POC version

### To Be Modified
- `crates/rue-parser/BUCK` - Update for insta
- `crates/rue/BUCK` - Update for insta
- `crates/rue/tests/cli_tests.rs` - Migrate to insta
- `CLAUDE.md` - Update testing documentation
- `docs/dev/snapshot-testing.md` - Rewrite for insta

### To Be Deleted
- `crates/rue-snapshot/` (entire directory)
- `INSTA_POC.md`
- `crates/rue-parser/tests/insta_utils.rs`
- `crates/rue-parser/tests/test_parser_snapshots_insta.rs`

## Rollback Plan

If issues are discovered:

1. The old `rue-snapshot` crate still exists
2. Old test files have been modified but not deleted
3. Can revert changes to BUCK files
4. Can revert changes to test files via git

This migration has been designed to be incremental and safe.

## Success Criteria

Migration is complete when:

- ✅ All tests using snapshots migrated to insta
- ✅ All tests passing with insta
- ✅ BUCK files updated and working
- ✅ `rue-snapshot` crate removed
- ✅ Documentation updated
- ✅ No `rue-snapshot` references remain in codebase
- ✅ CI passing (if applicable)
- ✅ Developer workflow documented and tested

## Estimated Completion

- **Infrastructure**: ✅ 100% complete
- **Parser Tests**: ✅ 75% complete
- **Corpus Tests**: ✅ 90% complete
- **CLI Tests**: 0% complete
- **BUCK Files**: 0% complete
- **Documentation**: 0% complete
- **Cleanup**: 0% complete

**Overall Progress**: ~60% complete

**Estimated Remaining Work**: 2-3 hours focused work

## Contact & Support

For questions about this migration:
- See `INSTA_POC.md` for original analysis and POC details
- Check `rue-insta-utils/src/lib.rs` for API documentation
- Consult https://insta.rs/docs/ for insta documentation
- Review migrated test files for patterns and examples
