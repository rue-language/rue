# Linker Issues and Investigation Plan

## Current Known Issues

### Critical Issues (Blocking Correctness)

1. **Entry Point Assumption** (`crates/rue-codegen/src/linker/mod.rs:248-251`)
   - Hardcodes `_start` assumption without verifying runtime provides it
   - No validation that entry point actually exists in linked runtime

2. **Section Alignment Bug** (`crates/rue-codegen/src/linker/mod.rs:394`)
   - Uses 16-byte alignment for .rodata but standard is typically 4 or 8
   - Could cause data access issues or performance problems

3. **Error Context Loss**
   - Errors converted to generic `CodegenError::Io` losing original context
   - Makes debugging linking failures difficult

### Architectural Issues

1. **Symbol Resolution Gaps**
   - No weak symbol support
   - Missing COMDAT/section group handling
   - No `.init_array`/`.fini_array` support for constructors/destructors

2. **Missing ELF Features**
   - No `.eh_frame` support (exception handling/unwinding)
   - No TLS support (thread-local storage)
   - No GOT/PLT for dynamic symbols

3. **Over-Abstraction**
   - Complex `SymbolResolutionPolicy` trait unnecessary for single runtime
   - Could simplify significantly

### Code Quality Issues

1. **Weak Typing**
   - Uses `(String, u64)` tuples instead of proper structs
   - Raw `u64` for addresses instead of newtype pattern
   - Strings where `&str` or `Cow<str>` would suffice

2. **Performance Issues**
   - Unnecessary cloning in symbol handling
   - Many small buffer extends instead of pre-allocation
   - String allocations in hot paths

3. **Large Functions**
   - `create_executable_elf` is 260+ lines
   - Needs decomposition into smaller, testable units

## Runtime Investigation Required

### Key Questions to Answer:

1. **Runtime Architecture**:
   - What is the relationship between `rue-crt0` and `rue-runtime`?
   - Is `rue-runtime` supposed to include `rue-crt0`?
   - Are we linking the complete runtime or just the startup code?

2. **Missing Symbols**:
   - What symbols does `rue-runtime` export that we might not be linking?
   - Are there runtime functions (print, memory management, etc.) that aren't being included?
   - Is the runtime being built correctly to include all necessary components?

3. **Build Configuration**:
   - How is the runtime being compiled?
   - Is it creating a proper relocatable object file with all symbols?
   - Are we linking against the right artifact?

### Investigation Steps:

1. Examine the `rue-crt0` and `rue-runtime` crate structures
2. Check BUCK build configurations for both
3. Analyze what symbols are actually in the compiled runtime object
4. Verify what the linker is actually linking
5. Check if user programs can access runtime functions

## Findings from Investigation

### Critical Runtime Discovery

**Root Cause of Crashes Found**: Debug tracing code in the `_start` function is causing "Illegal instruction" errors!

#### Runtime Architecture Issues

1. **Redundant Runtime Crates**:
   - `rue-crt0`: Complete runtime with all functions (startup, allocator, IO, conversions, formatting, memory, system)
   - `rue-runtime`: Currently only contains buffered_io functions - **should be the main runtime**
   - The compiler only links `rue-crt0` currently
   - Need to consolidate: move rue-crt0 content → rue-runtime, delete rue-crt0

2. **Debug Code in Production** ⚠️ **CRITICAL**:
   - `/workspace/crates/rue-crt0/src/startup.rs:118` contains `debug_trace()` calls
   - These execute immediately on program startup
   - Causes "Illegal instruction" crash in production builds
   - Debug syscalls are incompatible with minimal runtime environment

3. **Symbol Export Issues**:
   - Functions use `#[unsafe(no_mangle)]` which is correct Rust 2024 syntax ✓
   - Some functions may be missing proper `extern "C"` declarations
   - No visibility control attributes

4. **Build Configuration Problems**:
   - Building both rlib and staticlib versions creates confusion
   - Using `-Copt-level=0` for runtime (should use O2)
   - Missing LTO and other optimizations

### What's Actually Working

1. **Core Linking**: The linker correctly finds and processes all required symbols
2. **Symbol Resolution**: All `__rue_*` functions are properly exported
3. **Entry Point**: `_start` is correctly exported at offset 0x0
4. **Static Library**: The `.a` file contains all necessary sections (273) and symbols (88)

### Immediate Fixes Required

1. **Remove Debug Tracing** (CRITICAL):
   ```rust
   // Wrap debug calls in conditional compilation
   #[cfg(debug_assertions)]
   unsafe { debug_trace(b"[DEBUG] _start: entering\n") };
   ```

2. **Fix Function Declarations**:
   ```rust
   // Ensure all exported functions have extern "C":
   #[unsafe(no_mangle)]  // This is correct Rust 2024 syntax
   pub unsafe extern "C" fn __rue_function() {}
   ```

3. **Consolidate Runtime**:
   - Move all content from `rue-crt0` into `rue-runtime`
   - Delete `rue-crt0` crate after migration
   - Update all build references to use `rue-runtime`
   - This better reflects the fuller runtime scope beyond just CRT0

4. **Optimize Build**:
   ```
   rustc_flags = [
       "-Cpanic=abort",
       "-Copt-level=2",     // Not 0!
       "-Clto=thin",        // Enable LTO
       "-Ccodegen-units=1", // Better optimization
   ]
   ```

### Linker Assessment Summary

The linker implementation is **functionally adequate** but needs improvements:

**Good**:
- Correctly focuses on single runtime linking (not general-purpose)
- Successfully generates valid ELF64 executables
- Handles basic relocations (R_X86_64_64, R_X86_64_PC32)

**Issues**:
- Section alignment uses 16 bytes for .rodata (should be 4 or 8)
- Error handling loses context when converting to CodegenError::Io
- Over-abstracted for single runtime use case
- Large monolithic functions (create_executable_elf is 260+ lines)
- Weak typing with tuples instead of structs
- Unnecessary cloning and allocations

**The linking architecture itself is sound** - the crashes are purely from debug code in the runtime startup, not linker issues.