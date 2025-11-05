# Phase 6 Integration Notes

## Status: Phases 0-5 Complete ✅

**Date**: 2025-01-04

We have successfully completed the Rust runtime implementation through Phase 5. The runtime is fully functional and ready for integration with the compiler.

## What's Been Completed

### Phase 0: Preparation ✅
- Audited all 30+ runtime functions
- Documented performance baselines from Session 020
- Created `docs/sessions/021-rust-runtime/runtime-audit.md`

### Phase 1: Foundation ✅
- Created module structure (abi, syscall, conversion, memory, io)
- Set up no_std configuration with panic handler
- Defined constants and ABI documentation
- All modules compile successfully

### Phase 2: Syscalls ✅
- Implemented syscall wrappers (syscall0-3)
- Created `sys_write`, `sys_read`, `sys_exit` functions
- Refactored buffered_io to use centralized syscalls
- Added unit tests for error handling

### Phase 3: Conversions ✅
- Implemented `__rue_itoa` with i64::MIN handling
- Implemented `__rue_atoi` with whitespace and overflow handling
- 10+ comprehensive test cases
- Matches behavior of existing assembly implementation

### Phase 4: I/O ✅
- Implemented all `println` variants (i64, i32, bool, unit)
- Implemented `__rue_input` function
- Uses buffered stdout + conversion functions
- Clean composition of lower-level functions

### Phase 5: Memory Operations ✅
- Baseline implementations using Rust ptr methods
- ERMS-optimized variants using inline asm (rep movsb/stosb)
- CPU feature detection via CPUID
- Function pointer vtable for dynamic dispatch
- 4KB threshold for ERMS usage
- Comprehensive tests including overlap handling

## Runtime Function Summary

All functions are implemented and tested:

### Exported C ABI Functions

**I/O Operations:**
- `__rue_println_i64(value: i64)`
- `__rue_println_i32(value: i32)`
- `__rue_println_bool(value: bool)`
- `__rue_println_unit()`
- `__rue_input() -> i64`
- `__rue_write_byte(byte: u8)`
- `__rue_write_bytes(ptr: *const u8, len: usize)`
- `__rue_flush_stdout()`

**String Conversions:**
- `__rue_itoa(value: i64, buf: *mut u8) -> usize`
- `__rue_atoi(buf: *const u8, len: usize) -> i64`

**Memory Operations:**
- `__rue_memcpy(dst: *mut u8, src: *const u8, len: usize)`
- `__rue_memmove(dst: *mut u8, src: *const u8, len: usize)`
- `__rue_memset(dst: *mut u8, value: u8, len: usize)`
- `__rue_memzero(dst: *mut u8, len: usize)`

**Initialization:**
- `__rue_detect_cpu_features()`

## Build Status

The runtime builds successfully as a Rust library:
```bash
./buck2 build //crates/rue-runtime:rue-runtime
# Output: librue_runtime-0aab85fc.rmeta
```

## What Remains: Phase 6 Integration

Phase 6 is about connecting our Rust runtime to the compiler. This requires several substantial changes:

### 6.1 Buck2 Build Configuration

**Current state:**
- Runtime builds as `.rmeta` (Rust metadata)
- Preferred linkage is set to "static" in BUCK

**What's needed:**
- Configure Buck2 to output linkable format (`.rlib` or `.a`)
- Possibly need to use `rustc` flags for cdylib or staticlib
- May need custom Buck2 rule for cross-language linking

**Research needed:**
- How to build Rust cdylib with Buck2
- How to export C ABI symbols properly
- Whether to use .rlib (Rust) or .a (C-style static library)

### 6.2 Codegen Changes

**Current state:**
- Codegen generates runtime function bodies as assembly
- Located in `crates/rue-codegen/src/runtime/`
- Functions are embedded in `.text` section

**What's needed:**
- **Option A**: Add feature flag to switch between generated/linked runtime
  - Keep existing generation code temporarily
  - Add `--use-rust-runtime` flag
  - Allows gradual migration

- **Option B**: Full replacement
  - Remove runtime generation code
  - Assume symbols will be provided by linker
  - More aggressive but cleaner

**Files to modify:**
- `crates/rue-codegen/src/runtime/mod.rs` - Entry point
- `crates/rue-codegen/src/runtime/x86_64.rs` - Generate runtime call
- `crates/rue-codegen/src/target/x86_64/elf.rs` - ELF generation

**Recommended approach:** Option A (feature flag) for safety

### 6.3 Linking with Rue Programs

**Current state:**
- Rue programs are generated as complete ELF executables
- Runtime is embedded in the same ELF
- No external dependencies

**What's needed:**
- Use `crates/rue-codegen/src/linker/` (from Session 020)
- Link runtime object file with user code
- Resolve `__rue_*` symbols from runtime
- Apply relocations

**Integration points:**
```rust
// In codegen somewhere:
let mut linker = Linker::new();

// Add the runtime object file
linker.add_object_file_from_path("path/to/librue_runtime.a")?;

// Add user's generated code
linker.add_object_file("user_code".to_string(), &user_object_bytes)?;

// Link everything together
let linked = linker.link()?;

// Use linked.text_section, linked.rodata_section, etc. in ELF generation
```

**Challenges:**
1. **Runtime location**: Where is the runtime `.a` file at compile time?
   - Could be in buck-out
   - Need to pass path to compiler
   - Or embed in compiler binary?

2. **Symbol resolution**: Need to ensure all `__rue_*` symbols resolve
   - Linker from Session 020 should handle this
   - Need to test with actual symbols

3. **Initialization**: `__rue_detect_cpu_features` must run at startup
   - Need to ensure it's called before user `main`
   - Probably in `_start` or `__rue_main`

### 6.4 Testing Strategy

**Minimal test:**
1. Compile simple program: `fn main() { println(42); }`
2. Verify it calls `__rue_println_i64`
3. Verify symbol is resolved from runtime
4. Run the program, check output

**Full validation:**
1. Run all corpus tests with new runtime
2. Compare output to existing runtime
3. Verify no regressions
4. Measure code size and performance

## Technical Decisions to Make

### Decision 1: Static Library Format

**Options:**
- **`.rlib`**: Rust's native format, includes metadata
- **`.a`**: Standard static archive, C-compatible
- **`.o`**: Single object file

**Recommendation**: `.a` (static archive) because:
- C-compatible, standard format
- Linker from Session 020 expects ELF object files
- Easier to inspect with tools like `nm`, `objdump`

**Implementation**: May need to use `cargo rustc -- --crate-type=staticlib` or similar

### Decision 2: Runtime Distribution

**Options:**
- **Embed in compiler**: Runtime built into compiler binary
- **Ship separately**: Runtime as external file
- **Build on demand**: Compiler builds runtime when needed

**Recommendation**: Build on demand
- Runtime source is in `crates/rue-runtime/`
- Compiler can invoke Buck2 to build it
- Simplest for development

### Decision 3: Transition Strategy

**Options:**
- **Big bang**: Replace all codegen at once
- **Feature flag**: Support both runtimes temporarily
- **Gradual**: Migrate function by function

**Recommendation**: Feature flag approach
- Add `--use-rust-runtime` to compiler CLI
- Keep old codegen temporarily
- Allows testing and comparison
- Can remove old code after validation

### Decision 4: _start and Initialization

**Current state:**
- Runtime generates `_start` entry point
- Calls `__rue_main` which initializes and calls user `main`

**What's needed:**
- Need to ensure `__rue_detect_cpu_features` is called
- Need to set up any runtime state
- User `main` must be called correctly

**Options:**
- **Keep _start in codegen**: Generate startup code, call runtime init
- **Move _start to runtime**: Runtime provides complete startup sequence
- **Hybrid**: Codegen generates _start, runtime provides init function

**Recommendation**: Hybrid approach
- Codegen generates `_start` (knows user's `main` address)
- Runtime provides `__rue_init()` function that does:
  - CPU detection
  - Signal handler setup
  - Heap initialization
- `_start` calls `__rue_init()` then user `main`

## Next Steps

1. **Research Buck2 staticlib support**
   - Look at Buck2 docs for building C-compatible static libraries
   - Test building runtime as `.a`
   - Verify symbols are exported correctly

2. **Test linker with runtime**
   - Manually link a simple test
   - Verify symbol resolution works
   - Document any issues

3. **Design integration API**
   - Define how compiler will invoke linker
   - Design command-line interface
   - Plan error handling

4. **Implement feature flag**
   - Add `--use-rust-runtime` flag to CLI
   - Wire it through to codegen
   - Test both paths work

5. **End-to-end test**
   - Compile hello world with new runtime
   - Run and verify output
   - Iterate on any issues

## Estimated Effort

**Phase 6 remaining work**: ~4-8 hours
- Buck2 configuration: 1-2 hours
- Codegen modifications: 2-3 hours
- Linker integration: 1-2 hours
- Testing and debugging: 1-2 hours

**Phase 7 (Cleanup)**: ~2-3 hours
- Remove old runtime codegen
- Update documentation
- Final testing

**Phase 8 (Polish)**: ~1-2 hours
- Clippy and rustfmt
- Test coverage check
- Edge cases

**Total remaining**: ~7-13 hours

## References

- Session 014: `docs/sessions/014-runtime/`
- Session 020: `docs/sessions/020-runtime-refactor/`
- Object Linker: `crates/rue-codegen/src/linker/mod.rs`
- Runtime System: `docs/architecture/runtime-system.md`
