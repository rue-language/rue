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

### Phase 1: CPU Feature Detection & Vtable ✅
- [x] **1.1** Add vtable data structures
  - [x] **1.1a** Define function pointer slots in data section
  - [x] **1.1b** Add labels for each pointer
  - [x] **1.1c** Initialize with baseline function addresses
  
- [x] **1.2** Implement CPUID detection
  - [x] **1.2a** Add CPUID instruction wrapper
  - [x] **1.2b** Check for ERMS support (CPUID.7:EBX[9])
  - [x] **1.2c** Store CPU features in runtime context
  
- [x] **1.3** Wire up dynamic dispatch
  - [x] **1.3a** Set function pointers based on CPU features
  - [x] **1.3b** Update __rue_main to call CPU detection
  - [x] **1.3c** Test dispatch works correctly
  
- [x] **1.4** Create stub ERMS functions (temporary)
  - [x] **1.4a** __rue_memcpy_erms stub
  - [x] **1.4b** __rue_memmove_erms stub
  - [x] **1.4c** __rue_memset_erms stub

### Phase 2: Assembly Implementations ✅
*Note: Implemented via instruction emitter rather than separate .S files to avoid build complexity*

- [x] **2.1** Extend instruction emitter instead of .S files
  - [x] **2.1a** Add x86-64 instructions (TestRR, Loop, IncR, DecR, ShrRI)
  - [x] **2.1b** Add ERMS instructions (RepMovsq, RepStosq, RepMovsb, RepStosb)
  - [x] **2.1c** Add support instructions (Movzx8to32, ImulRI64, Std, MovMR16/MovRM16)
  
- [x] **2.2** Implement memcpy variants in Rust
  - [x] **2.2a** Enhanced baseline with size-optimized strategies
  - [x] **2.2b** ERMS version using rep movsb/movsq
  - [x] **2.2c** Tests pass with new implementations
  
- [x] **2.3** Implement memmove variants in Rust  
  - [x] **2.3a** Baseline with proper overlap handling (Std for backward)
  - [x] **2.3b** ERMS version with rep movsb
  - [x] **2.3c** Overlap cases handled correctly
  
- [x] **2.4** Implement memset variants in Rust
  - [x] **2.4a** Baseline with size strategies
  - [x] **2.4b** ERMS version using rep stosb/stosq
  - [x] **2.4c** Memzero wrapper implementation

*Future consideration: Separate .S files could provide more control but require:*
- *External assembler dependency (NASM/GAS)*
- *Complex build.rs modifications*
- *Buck2 build configuration updates*
- *Current approach maintains consistency with existing runtime architecture*

### Phase 3: Minimal Object Linker ✅
- [x] **3.1** Add ELF object file parser
  - [x] **3.1a** Parse ELF64 header
  - [x] **3.1b** Extract section headers
  - [x] **3.1c** Build symbol table from .symtab
  
- [x] **3.2** Implement relocation support
  - [x] **3.2a** Parse relocation entries
  - [x] **3.2b** Support R_X86_64_64 (absolute)
  - [x] **3.2c** Support R_X86_64_PC32 (PC-relative)
  
- [x] **3.3** Merge object sections
  - [x] **3.3a** Combine .text sections
  - [x] **3.3b** Combine .rodata sections
  - [x] **3.3c** Calculate .bss requirements
  
- [x] **3.4** Apply relocations
  - [x] **3.4a** Resolve symbols to addresses
  - [x] **3.4b** Patch relocation sites
  - [x] **3.4c** Validate final binary

### Phase 4: Hook Calls Through Pointers ✅
- [x] **4.1** Update code generation
  - [x] **4.1a** Change memcpy calls to use pointer
  - [x] **4.1b** Change memmove calls to use pointer (via runtime wrapper)
  - [x] **4.1c** Change memset calls to use pointer (via memzero)
  
- [x] **4.2** Add wrapper functions
  - [x] **4.2a** memzero wrapper (calls appropriate memset variant)
  - [x] **4.2b** Update all memzero call sites

### Phase 5: Buffered stdout (no_std Rust) ✅
- [x] **5.1** Create no_std crate structure
  - [x] **5.1a** crates/rue-runtime/Cargo.toml
  - [x] **5.1b** Configure for no_std, panic=abort (via runtime profile)
  - [x] **5.1c** Set up static library output (librue_runtime.a)
  
- [x] **5.2** Implement buffering logic
  - [x] **5.2a** Static 4KB buffer with atomic position tracking
  - [x] **5.2b** __rue_write_byte/__rue_write_bytes implementation
  - [x] **5.2c** __rue_flush_stdout implementation
  
- [x] **5.3** C ABI functions for assembly interop
  - [x] **5.3a** Proper calling convention support
  - [x] **5.3b** Auto-flush on newline and buffer full
  - [x] **5.3c** Direct write optimization for large outputs
  
- [x] **5.4** Error handling and testing
  - [x] **5.4a** Handle write failures (exit on error)
  - [x] **5.4b** Exit code 253 on I/O errors
  - [x] **5.4c** Test with assembly programs

### Phase 6: Testing & Benchmarking ✅
- [x] **6.1** Correctness test suite
  - [x] **6.1a** Property tests for memcpy (all sizes, alignments)
  - [x] **6.1b** Property tests for memmove (overlap cases)
  - [x] **6.1c** Property tests for memset
  - [x] **6.1d** Buffered I/O correctness tests
  
- [x] **6.2** Performance benchmarks
  - [x] **6.2a** Memory operation microbenchmarks
  - [x] **6.2b** Syscall count measurements
  - [x] **6.2c** End-to-end program benchmarks
  
- [x] **6.3** Integration testing
  - [x] **6.3a** Run full Rue test suite
  - [x] **6.3b** Test all sample programs
  - [x] **6.3c** Verify backward compatibility
  
- [x] **6.4** Documentation
  - [x] **6.4a** Document new runtime architecture
  - [x] **6.4b** Update performance numbers
  - [x] **6.4c** Add examples of optimization

### Phase 7: Full Integration & Cleanup
- [ ] **7.1** Integrate object linker
  - [ ] **7.1a** Use linker to link librue_runtime.a into all programs
  - [ ] **7.1b** Add rue-runtime as dependency in compiler
  - [ ] **7.1c** Update build process to build runtime library first
  - [ ] **7.1d** Test linking works with both Cargo and Buck2
  
- [ ] **7.2** Replace I/O implementations
  - [ ] **7.2a** Replace direct syscall println with __rue_write_bytes calls
  - [ ] **7.2b** Add __rue_flush_stdout call at program exit
  - [ ] **7.2c** Update print_int to use buffered I/O
  - [ ] **7.2d** Update any other I/O operations to use runtime
  
- [ ] **7.3** Remove old code
  - [ ] **7.3a** Delete old direct syscall implementations
  - [ ] **7.3b** Remove inline memory function implementations
  - [ ] **7.3c** Clean up any conditional compilation paths
  - [ ] **7.3d** Remove unused runtime function generation
  
- [ ] **7.4** Verify everything works
  - [ ] **7.4a** Run full test suite with new runtime
  - [ ] **7.4b** Verify all sample programs work
  - [ ] **7.4c** Check binary sizes are reasonable
  - [ ] **7.4d** Measure performance improvements
  
- [ ] **7.5** Documentation updates
  - [ ] **7.5a** Update architecture docs with new runtime design
  - [ ] **7.5b** Document the linker infrastructure
  - [ ] **7.5c** Update performance numbers in README
  - [ ] **7.5d** Add notes about buffered I/O behavior

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