# Session 021: Rust Runtime Rewrite - Implementation Plan

## Implementation Checklist

### Phase 0: Preparation and Setup (Session Start)
- [x] **0.1** Create session documentation directory
- [x] **0.2** Write design-decisions.md
- [x] **0.3** Write implementation-plan.md
- [ ] **0.4** Audit current runtime functionality (list all functions)
- [ ] **0.5** Document current performance baselines (run benchmarks)

### Phase 1: Runtime Library Foundation (Core Structure)
- [ ] **1.1** Set up new `rue-runtime` structure
  - [ ] **1.1a** Review existing `crates/rue-runtime/` structure
  - [ ] **1.1b** Create module organization (`lib.rs`, `io.rs`, `memory.rs`, `conversion.rs`, etc.)
  - [ ] **1.1c** Set up `#![no_std]` configuration
  - [ ] **1.1d** Define panic handler
  - [ ] **1.1e** Update Buck2 BUCK file for static library build
- [ ] **1.2** Define core ABI types and constants
  - [ ] **1.2a** Create `abi.rs` module with function signatures
  - [ ] **1.2b** Define constants (buffer sizes, alignment, etc.)
  - [ ] **1.2c** Document calling convention expectations
- [ ] **1.3** Set up testing infrastructure
  - [ ] **1.3a** Create unit test structure in rue-runtime
  - [ ] **1.3b** Set up test harness for no_std tests
  - [ ] **1.3c** Create integration test plan

### Phase 2: Syscall Foundation (Building Blocks)
- [ ] **2.1** Implement syscall wrappers
  - [ ] **2.1a** Create `syscall.rs` module
  - [ ] **2.1b** Implement `sys_write(fd, buf, len) -> Result<usize, i32>`
  - [ ] **2.1c** Implement `sys_read(fd, buf, len) -> Result<usize, i32>`
  - [ ] **2.1d** Implement `sys_exit(code) -> !`
  - [ ] **2.1e** Add error type and Result helpers
  - [ ] **2.1f** Write unit tests for syscall wrappers
- [ ] **2.2** Implement buffered stdout (migrate from existing)
  - [ ] **2.2a** Review existing `buffered_io.rs` implementation
  - [ ] **2.2b** Create new version with improvements
  - [ ] **2.2c** Implement `__rue_write_byte(byte: u8)`
  - [ ] **2.2d** Implement `__rue_write_bytes(buf: *const u8, len: usize)`
  - [ ] **2.2e** Implement `__rue_flush_stdout()`
  - [ ] **2.2f** Write unit tests for buffering logic
  - [ ] **2.2g** Test edge cases (full buffer, large writes, etc.)

### Phase 3: String Conversion Functions (Simple, Testable)
- [ ] **3.1** Implement `itoa` (integer to ASCII)
  - [ ] **3.1a** Write TDD test cases (positive, negative, zero, MIN, MAX)
  - [ ] **3.1b** Implement `__rue_itoa(value: i64, buf: *mut u8) -> usize`
  - [ ] **3.1c** Handle negative numbers with `-` prefix
  - [ ] **3.1d** Optimize for common cases
  - [ ] **3.1e** Verify all tests pass
  - [ ] **3.1f** Benchmark against old implementation
- [ ] **3.2** Implement `atoi` (ASCII to integer)
  - [ ] **3.2a** Write TDD test cases (valid, invalid, overflow, whitespace)
  - [ ] **3.2b** Implement `__rue_atoi(buf: *const u8, len: usize) -> i64`
  - [ ] **3.2c** Handle leading whitespace
  - [ ] **3.2d** Handle negative numbers
  - [ ] **3.2e** Handle overflow/underflow (return 0 or error?)
  - [ ] **3.2f** Verify all tests pass
  - [ ] **3.2g** Benchmark against old implementation

### Phase 4: I/O Functions (Build on Buffering)
- [ ] **4.1** Implement `println` variants
  - [ ] **4.1a** Implement `__rue_println_i64(value: i64)`
    - Uses `__rue_itoa` + `__rue_write_bytes` + newline
  - [ ] **4.1b** Implement `__rue_println_i32(value: i32)`
    - Cast to i64 and delegate
  - [ ] **4.1c** Implement `__rue_println_bool(value: bool)`
    - Write "true" or "false" + newline
  - [ ] **4.1d** Implement `__rue_println_unit()`
    - Write "()" + newline
  - [ ] **4.1e** Write unit tests for each variant
  - [ ] **4.1f** Write integration tests (verify output)
- [ ] **4.2** Implement input function
  - [ ] **4.2a** Write TDD test cases (various inputs)
  - [ ] **4.2b** Implement `__rue_input() -> i64`
    - Read from stdin (fd 0)
    - Parse using `__rue_atoi`
    - Handle newlines and whitespace
  - [ ] **4.2c** Test with valid and invalid inputs
  - [ ] **4.2d** Benchmark if relevant

### Phase 5: Memory Operations (Performance Critical)
- [ ] **5.1** Implement basic memory operations
  - [ ] **5.1a** Implement `__rue_memcpy(dst: *mut u8, src: *const u8, len: usize)`
    - Use `ptr::copy_nonoverlapping` for now
  - [ ] **5.1b** Implement `__rue_memmove(dst: *mut u8, src: *const u8, len: usize)`
    - Use `ptr::copy` (handles overlap)
  - [ ] **5.1c** Implement `__rue_memset(dst: *mut u8, value: u8, len: usize)`
    - Use `ptr::write_bytes`
  - [ ] **5.1d** Implement `__rue_memzero(dst: *mut u8, len: usize)`
    - Delegate to memset with 0
  - [ ] **5.1e** Write correctness tests (various sizes, alignments)
  - [ ] **5.1f** Write edge case tests (size 0, overlapping)
- [ ] **5.2** Add CPU feature detection
  - [ ] **5.2a** Create `cpuid.rs` module
  - [ ] **5.2b** Implement CPUID detection using inline asm
  - [ ] **5.2c** Detect ERMS support bit
  - [ ] **5.2d** Initialize feature flags at startup
  - [ ] **5.2e** Test on CPUs with/without ERMS (or mock)
- [ ] **5.3** Add optimized memory operations
  - [ ] **5.3a** Implement ERMS memcpy using inline asm
  - [ ] **5.3b** Implement ERMS memmove using inline asm
  - [ ] **5.3c** Implement ERMS memset using inline asm
  - [ ] **5.3d** Add threshold logic (use ERMS for size > 4KB)
  - [ ] **5.3e** Set up function pointer vtable
  - [ ] **5.3f** Initialize vtable based on CPU features
- [ ] **5.4** Benchmark and validate performance
  - [ ] **5.4a** Benchmark small copies (16, 64, 256 bytes)
  - [ ] **5.4b** Benchmark large copies (4KB, 64KB, 1MB)
  - [ ] **5.4c** Compare to old implementation
  - [ ] **5.4d** Verify within 5% or better
  - [ ] **5.4e** Profile with `perf` if needed

### Phase 6: Compiler Integration (Link Runtime)
- [ ] **6.1** Update Buck2 build configuration
  - [ ] **6.1a** Ensure rue-runtime builds to static library
  - [ ] **6.1b** Add rue-runtime as dependency to compiler
  - [ ] **6.1c** Configure linker to include runtime library
  - [ ] **6.1d** Test that library builds correctly
- [ ] **6.2** Update codegen to emit calls instead of definitions
  - [ ] **6.2a** Modify codegen to emit `call __rue_println_i64` etc.
  - [ ] **6.2b** Remove function body generation (keep both for now)
  - [ ] **6.2c** Add feature flag or config to switch between old/new
  - [ ] **6.2d** Test that calls are emitted correctly
- [ ] **6.3** Implement linking with static library
  - [ ] **6.3a** Review Session 020's object linker code
  - [ ] **6.3b** Implement static library linking in compiler
  - [ ] **6.3c** Resolve `__rue_*` symbols from library
  - [ ] **6.3d** Test symbol resolution
  - [ ] **6.3e** Test with simple test program
- [ ] **6.4** Integration testing
  - [ ] **6.4a** Compile simple "hello world" with new runtime
  - [ ] **6.4b** Compile test programs using each runtime function
  - [ ] **6.4c** Run corpus tests with new runtime
  - [ ] **6.4d** Verify all tests pass
  - [ ] **6.4e** Fix any issues discovered

### Phase 7: Cleanup and Migration Complete
- [ ] **7.1** Remove old assembly generation code
  - [ ] **7.1a** Remove `crates/rue-codegen/src/runtime/memory.rs`
  - [ ] **7.1b** Remove `crates/rue-codegen/src/runtime/memory_optimized.rs`
  - [ ] **7.1c** Remove `crates/rue-codegen/src/runtime/io.rs`
  - [ ] **7.1d** Remove `crates/rue-codegen/src/runtime/conversion.rs`
  - [ ] **7.1e** Remove `crates/rue-codegen/src/runtime/cpuid.rs`
  - [ ] **7.1f** Remove other now-unused runtime codegen modules
  - [ ] **7.1g** Update `mod.rs` to reflect removed modules
  - [ ] **7.1h** Clean up any unused imports or dead code
- [ ] **7.2** Update documentation
  - [ ] **7.2a** Update `docs/architecture/runtime-system.md`
  - [ ] **7.2b** Update `docs/implementation.md` if needed
  - [ ] **7.2c** Write session-summary.md for Session 021
  - [ ] **7.2d** Update CONTRIBUTING.md if build process changed
  - [ ] **7.2e** Add inline docs to rue-runtime modules
- [ ] **7.3** Performance validation
  - [ ] **7.3a** Run full benchmark suite
  - [ ] **7.3b** Compare to old implementation
  - [ ] **7.3c** Document any regressions or improvements
  - [ ] **7.3d** Profile if needed
- [ ] **7.4** Final testing
  - [ ] **7.4a** Run all unit tests (`./buck2 test //crates/rue-runtime:test`)
  - [ ] **7.4b** Run all compiler tests (`./buck2 test //crates/rue-compiler:test`)
  - [ ] **7.4c** Run all corpus tests (`./buck2 test //crates/rue:corpus_tests`)
  - [ ] **7.4d** Run all integration tests
  - [ ] **7.4e** Verify 100% pass rate

### Phase 8: Polish and Future-Proofing
- [ ] **8.1** Code quality improvements
  - [ ] **8.1a** Run clippy on rue-runtime: `./buck2 bxl //tools/bxl:clippy.bxl:check -- --targets //crates/rue-runtime:clippy`
  - [ ] **8.1b** Fix any clippy warnings
  - [ ] **8.1c** Run rustfmt: `./buck2 run //tools/rustfmt:fmt_fix`
  - [ ] **8.1d** Review code for unsafe usage, add safety comments
- [ ] **8.2** Test coverage analysis
  - [ ] **8.2a** Measure test coverage for rue-runtime
  - [ ] **8.2b** Add tests for uncovered code paths
  - [ ] **8.2c** Aim for >90% coverage
- [ ] **8.3** Edge case handling
  - [ ] **8.3a** Review error handling strategy
  - [ ] **8.3b** Test with unusual inputs (INT_MIN, INT_MAX, empty strings)
  - [ ] **8.3c** Test memory operations at page boundaries
  - [ ] **8.3d** Test with very large allocations
- [ ] **8.4** Documentation review
  - [ ] **8.4a** Ensure all public functions have doc comments
  - [ ] **8.4b** Add examples to complex functions
  - [ ] **8.4c** Document safety requirements for unsafe functions
  - [ ] **8.4d** Add module-level documentation

## Commit Strategy

**After each checklist item or small group of items:**
1. Run relevant tests to verify correctness
2. Update this file to check off completed items
3. Commit with descriptive message in imperative mood

**Example commit messages:**
- "Add syscall wrappers for write, read, exit"
- "Implement itoa with negative number support"
- "Add ERMS-optimized memcpy with feature detection"
- "Remove old assembly generation from codegen"

**Squashing**: Can squash related commits before creating PR, but keep granular during development for easier debugging.

## Testing Strategy per Phase

### During Development
- Run unit tests after each function: `./buck2 test //crates/rue-runtime:test`
- Run specific test: `./buck2 test //crates/rue-runtime:test -- test_name`

### Before Moving to Next Phase
- Run all runtime tests: `./buck2 test //crates/rue-runtime:test`
- Run compiler tests: `./buck2 test //crates/rue-compiler:test`
- Run relevant corpus tests: `./buck2 test //crates/rue:corpus_tests`

### Before Phase 7 Cleanup
- **Full test suite**: `./buck2 test //crates/...`
- **Format check**: `./buck2 test //tools/rustfmt:fmt_check_test`
- **Clippy**: `./buck2 bxl //tools/bxl:clippy.bxl:all`

## Benchmarking Strategy

### Baseline Measurement (Phase 0)
Before starting, measure current performance:
- Run existing benchmarks or create simple ones
- Record results for comparison

### Per-Function Benchmarking (Phases 3-5)
For each migrated function:
- Create simple benchmark (e.g., 1M iterations)
- Compare new vs old implementation
- Accept if within 5% or better

### Final Validation (Phase 7)
- Run full benchmark suite
- Compare overall program performance
- Document any changes

## Rollback Plan

If issues arise:
- Each phase is independently committable
- Can revert to previous phase
- Can keep old codegen path with feature flag during transition
- Don't remove old code until 100% confident in new implementation

## Success Criteria (from design-decisions.md)

- [x] All runtime functions reimplemented in Rust
- [ ] All corpus tests pass with new runtime
- [ ] Performance within 5% of old implementation (or better)
- [ ] Code size within 10% of old implementation (or better)
- [ ] Runtime has >90% test coverage
- [ ] All assembly generation removed from codegen
- [ ] Documentation updated (architecture, implementation guide)
- [ ] Can compile and run all test programs

## Notes

- This plan prioritizes safety and correctness over speed
- Incremental approach allows catching issues early
- Each phase builds on previous work
- Can pause at any phase boundary if needed
- Document any deviations from this plan in session notes
