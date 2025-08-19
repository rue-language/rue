# Implementation Plan: Runtime Refactor

## Implementation Checklist

### Phase 0: Builder Hygiene [Immediate Quick Wins] ✅
- [x] **0.1** Update allocator alignment to 16 bytes
  - [x] **0.1a** Modify alignment calculation in `generate_alloc_function`
  - [x] **0.1b** Update alignment mask from -8 to -16
  - [x] **0.1c** Test allocator with various sizes
  
- [x] **0.2** Remove excessive register saves
  - [x] **0.2a** Audit all runtime functions for unnecessary push/pop
  - [x] **0.2b** Remove caller-saved register preservation (RCX, RDX, R8-R11)
  - [x] **0.2c** Keep only callee-saved registers where needed
  
- [x] **0.3** Implement red zone usage
  - [x] **0.3a** Identify leaf functions (no calls)
  - [x] **0.3b** Use RSP-128 to RSP for scratch space (N/A - no scratch needed)
  - [x] **0.3c** Remove unnecessary frame setup in leaf functions
  
- [x] **0.4** Run tests to verify no regressions

### Phase 1: CPU Feature Detection & Vtable
- [ ] **1.1** Add vtable data structures
  - [ ] **1.1a** Define function pointer slots in data section
  - [ ] **1.1b** Add labels for each pointer
  - [ ] **1.1c** Initialize with baseline function addresses
  
- [ ] **1.2** Implement CPUID detection
  - [ ] **1.2a** Add CPUID instruction wrapper
  - [ ] **1.2b** Check for ERMS support (CPUID.7:EBX[9])
  - [ ] **1.2c** Store CPU features in runtime context
  
- [ ] **1.3** Wire up dynamic dispatch
  - [ ] **1.3a** Set function pointers based on CPU features
  - [ ] **1.3b** Update __rue_main to call CPU detection
  - [ ] **1.3c** Test dispatch works correctly
  
- [ ] **1.4** Create stub ERMS functions (temporary)
  - [ ] **1.4a** __rue_memcpy_erms stub
  - [ ] **1.4b** __rue_memmove_erms stub
  - [ ] **1.4c** __rue_memset_erms stub

### Phase 2: Assembly Blobs
- [ ] **2.1** Set up assembly infrastructure
  - [ ] **2.1a** Create runtime/asm/x86_64/ directory
  - [ ] **2.1b** Add build.rs to compile .S files
  - [ ] **2.1c** Update BUCK build configuration
  
- [ ] **2.2** Implement memcpy variants
  - [ ] **2.2a** memcpy_baseline.S with size-optimized strategies
  - [ ] **2.2b** memcpy_erms.S using rep movsb
  - [ ] **2.2c** Correctness tests for both variants
  
- [ ] **2.3** Implement memmove variants
  - [ ] **2.3a** memmove_baseline.S with overlap handling
  - [ ] **2.3b** memmove_erms.S with rep movsb
  - [ ] **2.3c** Property tests for overlap cases
  
- [ ] **2.4** Implement memset variants
  - [ ] **2.4a** memset_baseline.S with size strategies
  - [ ] **2.4b** memset_erms.S using rep stosb
  - [ ] **2.4c** Add memzero wrapper

### Phase 3: Minimal Object Linker
- [ ] **3.1** Add ELF object file parser
  - [ ] **3.1a** Parse ELF64 header
  - [ ] **3.1b** Extract section headers
  - [ ] **3.1c** Build symbol table from .symtab
  
- [ ] **3.2** Implement relocation support
  - [ ] **3.2a** Parse relocation entries
  - [ ] **3.2b** Support R_X86_64_64 (absolute)
  - [ ] **3.2c** Support R_X86_64_PC32 (PC-relative)
  
- [ ] **3.3** Merge object sections
  - [ ] **3.3a** Combine .text sections
  - [ ] **3.3b** Combine .rodata sections
  - [ ] **3.3c** Calculate .bss requirements
  
- [ ] **3.4** Apply relocations
  - [ ] **3.4a** Resolve symbols to addresses
  - [ ] **3.4b** Patch relocation sites
  - [ ] **3.4c** Validate final binary

### Phase 4: Hook Calls Through Pointers
- [ ] **4.1** Update code generation
  - [ ] **4.1a** Change memcpy calls to use pointer
  - [ ] **4.1b** Change memmove calls to use pointer
  - [ ] **4.1c** Change memset calls to use pointer
  
- [ ] **4.2** Add wrapper functions
  - [ ] **4.2a** memzero wrapper (calls memset with 0)
  - [ ] **4.2b** Update all memzero call sites
  
- [ ] **4.3** Performance validation
  - [ ] **4.3a** Benchmark indirect vs direct calls
  - [ ] **4.3b** Verify ERMS performance improvement
  - [ ] **4.3c** Check code size impact

### Phase 5: Buffered stdout (no_std Rust)
- [ ] **5.1** Create no_std crate structure
  - [ ] **5.1a** runtime/rust/no_std_stdout/Cargo.toml
  - [ ] **5.1b** Configure for no_std, panic=abort
  - [ ] **5.1c** Set up static library output
  
- [ ] **5.2** Implement buffering logic
  - [ ] **5.2a** Static 4KB buffer in .bss
  - [ ] **5.2b** __rue_write implementation
  - [ ] **5.2c** __rue_flush implementation
  
- [ ] **5.3** Update println functions
  - [ ] **5.3a** Use __rue_write instead of direct syscall
  - [ ] **5.3b** Remove per-write syscalls
  - [ ] **5.3c** Add flush at program exit
  
- [ ] **5.4** Error handling
  - [ ] **5.4a** Handle write failures
  - [ ] **5.4b** Exit on I/O errors
  - [ ] **5.4c** Test error paths

### Phase 6: Testing & Benchmarking
- [ ] **6.1** Correctness test suite
  - [ ] **6.1a** Property tests for memcpy (all sizes, alignments)
  - [ ] **6.1b** Property tests for memmove (overlap cases)
  - [ ] **6.1c** Property tests for memset
  - [ ] **6.1d** Buffered I/O correctness tests
  
- [ ] **6.2** Performance benchmarks
  - [ ] **6.2a** Memory operation microbenchmarks
  - [ ] **6.2b** Syscall count measurements
  - [ ] **6.2c** End-to-end program benchmarks
  
- [ ] **6.3** Integration testing
  - [ ] **6.3a** Run full Rue test suite
  - [ ] **6.3b** Test all sample programs
  - [ ] **6.3c** Verify backward compatibility
  
- [ ] **6.4** Documentation
  - [ ] **6.4a** Document new runtime architecture
  - [ ] **6.4b** Update performance numbers
  - [ ] **6.4c** Add examples of optimization

### Phase 7: Rollout
- [ ] **7.1** Feature flag implementation
  - [ ] **7.1a** Add --new-runtime flag
  - [ ] **7.1b** Conditional compilation paths
  - [ ] **7.1c** A/B testing capability
  
- [ ] **7.2** Gradual rollout
  - [ ] **7.2a** Enable for memory operations only
  - [ ] **7.2b** Enable for I/O operations
  - [ ] **7.2c** Make default
  
- [ ] **7.3** Cleanup
  - [ ] **7.3a** Remove old implementations
  - [ ] **7.3b** Remove feature flags
  - [ ] **7.3c** Final documentation update

## Time Estimates

| Phase | Estimated Time | Complexity |
|-------|---------------|------------|
| Phase 0 | 2-3 hours | Low |
| Phase 1 | 3-4 hours | Medium |
| Phase 2 | 6-8 hours | High |
| Phase 3 | 8-10 hours | High |
| Phase 4 | 2-3 hours | Low |
| Phase 5 | 4-5 hours | Medium |
| Phase 6 | 4-5 hours | Medium |
| Phase 7 | 2-3 hours | Low |

**Total: 31-41 hours**

## Risk Mitigation

1. **Each phase independently testable** - Can stop at any phase
2. **Feature flags** - Can roll back if issues found
3. **Extensive testing** - Property tests catch edge cases
4. **Incremental changes** - Small, reviewable PRs

## Success Criteria

- [ ] All existing tests pass
- [ ] ERMS path 2x faster for large copies
- [ ] Buffered I/O reduces syscalls by >99%
- [ ] No performance regressions
- [ ] Clean, maintainable code

## Next Steps

1. Start with Phase 0 (quick wins, low risk)
2. Set up assembly build infrastructure
3. Implement CPU detection
4. Begin incremental rollout

## Notes

- Keep commits small and focused
- Run tests after each step
- Document assembly code thoroughly
- Benchmark before/after each optimization
- Update this checklist as work progresses