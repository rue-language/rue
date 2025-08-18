# Design Decisions: Runtime Refactor

## Architecture Decisions

### 1. Hybrid Runtime Approach
**Decision**: Use a hybrid of assembly, Rust codegen, and no_std Rust
**Rationale**:
- Assembly for hot paths (memcpy, memmove, memset) - maximum performance
- Rust codegen for startup, syscalls, simple functions - maintainable, no external deps
- no_std Rust for complex policies (buffered I/O) - safe, easier to maintain

**Alternatives Considered**:
- Pure assembly: Too hard to maintain, error-prone
- Pure Rust codegen: Can't achieve optimal performance for memory ops
- External libc: Violates "no external linker" constraint

### 2. CPU Feature Detection Strategy
**Decision**: Runtime detection via CPUID, dispatch through function pointers
**Rationale**:
- Single binary works on all x86-64 CPUs
- Near-zero overhead (one indirect call)
- Easy to extend for future optimizations (AVX2, AVX-512)

**Alternatives Considered**:
- Compile-time selection: Would need multiple binaries
- Dynamic code generation: Too complex, security concerns
- Always use baseline: Leaves performance on the table

### 3. Memory Operation Strategies

#### Small (< 32 bytes)
- Unrolled byte/word/dword moves
- Inline in hot paths
- Minimal branch overhead

#### Medium (32-256 bytes)
- Qword (8-byte) moves with tail handling
- Balance between code size and performance
- Good cache behavior

#### Large (> 256 bytes)
- ERMS: `rep movsb` on modern CPUs (2013+)
- Baseline: `rep movsq` with byte tail
- ERMS is often faster than hand-rolled loops on modern CPUs

### 4. Object File Integration
**Decision**: Minimal ELF relocatable object support
**Rationale**:
- Only support what we need (R_X86_64_64, R_X86_64_PC32)
- No need for full linker complexity
- Keeps binary size small

**Alternatives Considered**:
- Use system linker: Violates no-external-deps constraint
- Inline all assembly: Can't use standard assembler, harder to maintain
- Pre-assembled binary blobs: Platform-specific, hard to rebuild

### 5. Buffered I/O Design
**Decision**: Fixed 4KB buffer in .bss, flush on newline or full
**Rationale**:
- 4KB matches typical page size
- Line buffering matches expected behavior
- Static allocation avoids heap complexity
- Simple to implement correctly

**Trade-offs**:
- Fixed buffer size (not growable)
- Single global buffer (not thread-safe, but Rue is single-threaded)
- No async I/O support (not needed for Rue's use case)

### 6. ABI Stability
**Decision**: Freeze internal runtime ABI in v1
**Rationale**:
- Allows independent evolution of components
- Enables testing different implementations
- Clear contract for assembly code

**ABI Choices**:
- SysV x86-64 calling convention (standard)
- No stack alignment requirements beyond SysV
- Caller-saved registers per SysV
- Red zone usage allowed in leaf functions

### 7. Testing Strategy
**Decision**: Property-based tests + microbenchmarks
**Rationale**:
- Property tests catch edge cases (overlap, alignment)
- Microbenchmarks verify optimization effectiveness
- Integration tests ensure compatibility

**Test Coverage**:
- Correctness: All sizes, alignments, overlaps
- Performance: Key size buckets, ERMS vs baseline
- Compatibility: All existing Rue programs must work

## Implementation Principles

1. **Incremental**: Each phase independently testable
2. **Feature-flagged**: New runtime behind flag initially
3. **Backward compatible**: Existing programs continue to work
4. **Performance-first**: Optimize hot paths aggressively
5. **Maintainable**: Clear separation of concerns
6. **Documented**: Assembly code heavily commented
7. **Tested**: Comprehensive test coverage before default

## Risk Analysis

### Technical Risks
1. **Relocation complexity**: Mitigated by supporting minimal set
2. **Assembly bugs**: Mitigated by extensive testing
3. **Performance regression**: Mitigated by benchmarking
4. **Platform compatibility**: Focus on x86-64 Linux only

### Project Risks
1. **Scope creep**: Strictly follow phase plan
2. **Over-optimization**: Focus on measurable improvements
3. **Maintenance burden**: Keep assembly minimal, document well

## Success Metrics

1. **Performance**
   - ERMS memcpy ≥ 2x faster for large copies
   - Buffered I/O reduces syscalls by >99%
   - No regression on existing benchmarks

2. **Correctness**
   - All existing tests pass
   - Property tests pass with millions of cases
   - No memory safety issues

3. **Maintainability**
   - Clear module boundaries
   - Well-documented interfaces
   - Easy to add new optimizations

## Future Extensions

Once this foundation is in place, we can consider:
1. AVX2/AVX-512 memory operations
2. SIMD string operations
3. Custom allocator (arena, pool)
4. Profile-guided optimization
5. Link-time optimization integration
6. Cross-platform support (ARM64)