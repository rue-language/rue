# Session 021: Rust Runtime Rewrite - Design Decisions

## Context

Rue currently uses a hybrid runtime approach:
- Code generation in `crates/rue-codegen/src/runtime/` generates x86-64 assembly
- Small `rue-runtime` crate provides buffered I/O
- Assembly code is embedded into every compiled executable

This approach has significant drawbacks:
- Not idiomatic for modern compilers
- Hard to maintain (assembly in Rust strings)
- Hard to test (can't unit test functions in isolation)
- Wasteful (regenerates same code every compilation)
- Less portable (architecture code mixed into codegen)

## Goal

Rewrite Rue's runtime as a proper, idiomatic compiler runtime library:
- Separate `rue-runtime` crate in Rust
- Compile once, link many times
- Clean ABI boundary between generated code and runtime
- Follow patterns from Rust, Go, Zig, and other modern compilers

## Key Design Decisions

### Decision 1: 100% Rust Implementation

**Decision**: Implement the entire runtime in Rust, using inline assembly only where absolutely necessary.

**Rationale**:
- Rust provides excellent abstractions that don't cost performance
- LLVM's optimizer can often match or beat hand-written assembly
- Inline assembly in Rust is well-supported and easier to maintain than separate `.s` files
- Can leverage Rust's safety guarantees for most code
- Easier to test, benchmark, and maintain

**Alternatives Considered**:
- Hybrid Rust + separate assembly files: More complex build, harder to maintain
- Keep current codegen approach: Doesn't solve architectural issues
- Use C for runtime: Requires libc, loses Rust's safety benefits

**Trade-offs**:
- May need careful benchmarking to ensure performance matches hand-crafted asm
- Learning curve for inline asm syntax if needed
- But: Much better maintainability, testability, and portability

### Decision 2: Static Library Linkage Model

**Decision**: Compile `rue-runtime` to a static library (`.a` or `.o`) that the compiler links against.

**Rationale**:
- Standard approach for all modern compilers (Rust, Go, Zig, etc.)
- No dynamic linking overhead or complexity
- Single executable output (matches current behavior)
- Easier distribution (no separate runtime to install)
- Buck2 already has good support for this

**Alternatives Considered**:
- Dynamic library (`.so`): Adds complexity, startup overhead, distribution issues
- Embedding object code in compiler binary: Weird, hard to update runtime independently
- Keep generating assembly: We're trying to move away from this

**Implementation Notes**:
- Use `#[no_mangle]` and `extern "C"` for exported functions
- Build with Buck2 as `rust_library` with `crate_type = "staticlib"`
- Compiler locates library at link time and resolves symbols

### Decision 3: Runtime CPU Feature Detection

**Decision**: Keep runtime CPU feature detection with vtable dispatch (from Session 020).

**Rationale**:
- Single binary works on all x86-64 CPUs (wide compatibility)
- Optimal performance on newer CPUs with ERMS support
- Detection cost is one-time at program startup
- Session 020 already implemented this infrastructure well

**Alternatives Considered**:
- Compile-time feature selection: Requires distributing multiple binaries
- No optimization: Leaves 2-4x performance on table for memory operations
- Always use ERMS: Breaks on older CPUs

**Implementation**:
- Use inline assembly for CPUID detection
- Function pointer vtable for dispatching to optimal implementation
- Initialize once in `__rue_init` or similar startup function

### Decision 4: Clean ABI Boundary

**Decision**: Define a clear, stable ABI between generated code and runtime.

**Runtime Functions** (all `extern "C"`):
- **Memory**: `__rue_memcpy`, `__rue_memmove`, `__rue_memset`, `__rue_memzero`
- **I/O**: `__rue_println_i64`, `__rue_println_bool`, etc., `__rue_input`
- **Buffering**: `__rue_write_byte`, `__rue_write_bytes`, `__rue_flush_stdout`
- **Conversion**: `__rue_itoa`, `__rue_atoi`
- **Lifecycle**: `__rue_init`, `__rue_exit`
- **Allocation**: `__rue_malloc`, `__rue_free` (future)

**Calling Convention**: System V AMD64 ABI (standard for Linux x86-64)

**Rationale**:
- Clear contract between compiler and runtime
- Easier to evolve each independently
- Could theoretically swap runtimes (e.g., testing mock runtime)
- Standard C ABI makes debugging easier (gdb understands it)

### Decision 5: No Standard Library Dependencies

**Decision**: `rue-runtime` remains `#![no_std]` with direct syscalls.

**Rationale**:
- No libc dependency = smaller binaries, faster startup
- Full control over behavior and error handling
- Matches Rue's systems programming philosophy
- Already working well in Session 020's buffered I/O

**Implementation**:
- Use `syscall` crate or inline `syscall` instructions
- Define our own minimal panic handler
- Static buffer for stdout (no allocations)

### Decision 6: Incremental Migration Strategy

**Decision**: Migrate function-by-function rather than big bang rewrite.

**Migration Order**:
1. **Phase 1**: Set up new runtime structure, keep old codegen working
2. **Phase 2**: Migrate simple functions (itoa, atoi)
3. **Phase 3**: Migrate I/O functions (println variants, input)
4. **Phase 4**: Migrate memory operations (memcpy, memmove, memset)
5. **Phase 5**: Integrate CPU detection and optimization
6. **Phase 6**: Clean up old codegen, remove assembly generation
7. **Phase 7**: Update linker to use static library

**Rationale**:
- Lower risk (can test each function independently)
- Can commit working code frequently
- Easier to debug issues (smaller changes)
- Can benchmark before/after for each function

**Trade-offs**:
- Takes longer than big bang
- Temporary duplication during transition
- But: Much safer, easier to review, easier to fix issues

### Decision 7: Testing Strategy

**Decision**: Comprehensive testing at multiple levels.

**Test Levels**:
1. **Unit tests**: Test each runtime function in isolation (in rue-runtime crate)
2. **Integration tests**: Test compiler calling runtime functions
3. **Corpus tests**: Existing corpus tests validate end-to-end behavior
4. **Benchmarks**: Performance validation (using criterion or similar)

**Specific Tests**:
- Edge cases (empty strings, zero sizes, large values)
- Error handling (invalid input, syscall failures)
- Performance regression tests (compare to old implementation)
- Alignment requirements (for memory operations)
- Cross-function integration (e.g., println uses itoa uses write)

**Rationale**:
- Runtime bugs are critical (crash programs)
- Performance regressions are unacceptable
- Need confidence before removing old implementation
- Good tests enable fearless refactoring

## Architecture Overview

```
┌───────────────────────────────────────────────────────┐
│                   User Rue Program                    │
│                                                       │
│   let x = 42;                                        │
│   println(x);                                        │
└───────────────────────────────────────────────────────┘
                         │
                         ↓ compiled by
┌───────────────────────────────────────────────────────┐
│                  rue-compiler                         │
│                                                       │
│  - Generates user code (mov, add, call, etc.)       │
│  - Emits calls to runtime: "call __rue_println_i64"  │
│  - Does NOT generate runtime function bodies         │
└───────────────────────────────────────────────────────┘
                         │
                         ↓ links with
┌───────────────────────────────────────────────────────┐
│              rue-runtime (static library)             │
│                                                       │
│  Exports (extern "C"):                               │
│    - __rue_println_i64(value: i64)                   │
│    - __rue_input() -> i64                            │
│    - __rue_memcpy(dst, src, len)                     │
│    - __rue_itoa(value: i64, buf: *mut u8) -> usize   │
│    - ... etc                                          │
│                                                       │
│  Implementation:                                      │
│    - Pure Rust (unsafe where needed)                 │
│    - Inline asm for CPUID, syscalls if beneficial    │
│    - no_std, direct syscalls                         │
│    - Static buffers (4KB for stdout)                 │
└───────────────────────────────────────────────────────┘
                         │
                         ↓ syscalls to
┌───────────────────────────────────────────────────────┐
│                    Linux Kernel                       │
│                                                       │
│  System calls: write, read, exit, mmap, etc.         │
└───────────────────────────────────────────────────────┘
```

## Performance Considerations

### Memory Operations

**Current Performance** (from Session 020):
- Baseline: ~1 cycle per byte
- ERMS: ~0.25 cycles per byte on supporting CPUs
- Detection overhead: ~100 cycles once at startup

**Target Performance**:
- Match or beat current implementation
- Leverage Rust's `ptr::copy_nonoverlapping` (LLVM optimizes well)
- Keep ERMS path for large copies (>4KB)
- Benchmark threshold for dispatch decision

### I/O Operations

**Current**: Direct syscalls with some buffering

**Target**:
- 4KB stdout buffer (keep from Session 020)
- Batch small writes
- Auto-flush on newline or buffer full
- Zero-copy for large writes (>4KB)

### Code Size

**Current**: < 2KB runtime overhead per executable

**Target**: Maintain or reduce code size
- Rust's dead code elimination should help
- Inline small functions
- Share code between similar functions (e.g., println variants)

## Migration Risks and Mitigations

### Risk 1: Performance Regression

**Mitigation**:
- Benchmark before and after each function migration
- Keep old implementation available during transition
- Can fall back if performance issues arise
- Profile with `perf` to identify hot spots

### Risk 2: Breaking Existing Tests

**Mitigation**:
- Run full corpus test suite after each change
- Incremental migration allows quick rollback
- Commit frequently with working states

### Risk 3: Linker Complexity

**Mitigation**:
- Session 020's object linker work provides foundation
- Test linking process independently before integration
- Start with simple functions (fewer relocations)

### Risk 4: ABI Stability

**Mitigation**:
- Document ABI clearly in this document
- Add tests that verify function signatures
- Version the runtime if we need breaking changes

## Success Criteria

This rewrite will be considered successful when:

1. ✅ All runtime functions reimplemented in Rust
2. ✅ All corpus tests pass with new runtime
3. ✅ Performance within 5% of old implementation (or better)
4. ✅ Code size within 10% of old implementation (or better)
5. ✅ Runtime has >90% test coverage
6. ✅ All assembly generation removed from codegen
7. ✅ Documentation updated (architecture, implementation guide)
8. ✅ Can compile and run all test programs

## Future Enhancements

Once the base runtime is stable, we can consider:

- **SIMD optimizations**: Use Rust's portable SIMD once stable
- **More sophisticated buffering**: Read buffering, stdio buffering
- **Error handling**: More granular error reporting
- **Allocation improvements**: Better malloc/free implementation
- **Additional architectures**: ARM64 support (easier with Rust)
- **Runtime flags**: Enable/disable features at compile time

## References

- Session 014: Runtime and I/O initial implementation
- Session 020: Runtime refactor with ERMS and buffered I/O
- `docs/architecture/runtime-system.md`: Current runtime architecture
- Rust `core::arch`: Inline assembly and intrinsics documentation
- System V AMD64 ABI: Calling convention specification
