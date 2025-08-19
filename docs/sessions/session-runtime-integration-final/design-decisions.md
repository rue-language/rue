# Design Decisions: Runtime Integration Architecture

## Date: 2025-08-19

## Context
The Rue compiler had a complex runtime integration system that manually generated runtime code and merged it with user code using brittle offset calculations. This needed to be replaced with a proper linking architecture.

## Key Design Decisions

### 1. Object Model Design

**Decision**: Create a structured `AsmObject` model with sections, symbols, and relocations.

**Alternatives Considered**:
- Continue with raw byte arrays and offset tracking
- Use goblin's object structures directly
- Create a simpler flat model without sections

**Rationale**:
- Structured model provides type safety and prevents errors
- Sections allow proper memory permission handling (executable, writable)
- Relocations enable position-independent code
- Compatible with standard ELF tooling for debugging

### 2. Separate Assembler and Linker

**Decision**: Split assembly and linking into distinct phases with clear interfaces.

**Alternatives Considered**:
- Combined assembler-linker like the old system
- Direct machine code generation without intermediate objects
- Use external assembler (nasm) and linker (ld)

**Rationale**:
- Separation of concerns improves maintainability
- Allows testing each component independently
- Standard compiler architecture used by GCC, LLVM
- Enables future features like incremental compilation

### 3. Two-Pass Assembly

**Decision**: Use two-pass assembly to resolve forward references.

**Alternatives Considered**:
- Single pass with backpatching
- Three-pass with optimization phase
- Lazy resolution during linking

**Rationale**:
- Two passes sufficient for label resolution
- First pass collects all labels and sizes
- Second pass can generate correct relocations
- Simpler than backpatching, more efficient than three-pass

### 4. Runtime Library vs Generated Runtime

**Decision**: Support both external runtime library and fallback generated runtime.

**Alternatives Considered**:
- Always require external library
- Always generate runtime
- Compile-time choice via feature flags

**Rationale**:
- Flexibility for different deployment scenarios
- External library allows C/Rust runtime implementation
- Generated runtime ensures self-contained compiler
- Runtime detection at compile time avoids configuration

### 5. Symbol Resolution Strategy

**Decision**: Use standard strong/weak symbol binding with duplicate detection.

**Alternatives Considered**:
- Allow duplicate symbols with priority rules
- Require unique symbols everywhere
- Use namespace prefixes to avoid conflicts

**Rationale**:
- Standard ELF semantics familiar to developers
- Strong symbols prevent accidental overrides
- Weak symbols allow default implementations
- Early duplicate detection prevents subtle bugs

### 6. Relocation Types

**Decision**: Support minimal set of relocations (PC32, Abs64, Abs32).

**Alternatives Considered**:
- Full x86-64 relocation set
- Only PC-relative relocations
- Custom relocation types

**Rationale**:
- Minimal set covers all current use cases
- PC32 for calls and branches
- Abs64 for data pointers
- Abs32 for compact data
- Can extend as needed without breaking changes

### 7. Archive Member Extraction

**Decision**: Iterative extraction based on undefined symbols.

**Alternatives Considered**:
- Extract all members upfront
- Manual member specification
- Dependency graph analysis

**Rationale**:
- Standard linker behavior (ld, lld)
- Minimizes final executable size
- Handles circular dependencies correctly
- Efficient for large libraries

### 8. Section Merging Strategy

**Decision**: Merge sections by name with alignment handling.

**Alternatives Considered**:
- Keep sections separate
- Merge by attributes (executable, writable)
- Custom section ordering rules

**Rationale**:
- Standard ELF convention (.text, .data, .bss)
- Simplifies address assignment
- Maintains alignment requirements
- Compatible with debuggers and tools

### 9. Error Handling Philosophy

**Decision**: Fail fast with clear error messages.

**Alternatives Considered**:
- Best-effort linking with warnings
- Interactive error resolution
- Automatic fallbacks

**Rationale**:
- Clear errors prevent subtle runtime bugs
- Developers can fix issues immediately
- No surprising behavior in production
- Matches Rust's error handling philosophy

### 10. Pipeline Integration

**Decision**: Minimal changes to existing pipeline, drop-in replacement.

**Alternatives Considered**:
- Complete pipeline rewrite
- Gradual migration with feature flags
- Parallel old and new pipelines

**Rationale**:
- Reduces risk of breaking existing functionality
- Allows incremental testing
- Same interface means no caller changes
- Can revert if issues discovered

## Trade-offs Accepted

1. **Performance** - Two-pass assembly slower than single pass, but correctness more important
2. **Memory** - Keeping all objects in memory vs streaming, but simplifies implementation
3. **Features** - No dynamic linking initially, can add later if needed
4. **Debugging** - Limited debug symbol support, focusing on correctness first
5. **Optimization** - No link-time optimization, keeping linker simple

## Future Considerations

1. **Incremental Linking** - Cache linked objects for faster rebuilds
2. **LTO Support** - Link-time optimization for better performance
3. **Debug Symbols** - DWARF generation for better debugging
4. **Dynamic Linking** - Shared library support if needed
5. **Parallelization** - Parallel section merging and relocation application

## Validation

The design was validated by:
1. Successfully compiling test programs
2. Proper symbol resolution in multi-object scenarios
3. Correct relocation application
4. Archive member extraction working as expected
5. Clean separation allowing unit testing of components

This architecture provides a solid foundation for future enhancements while maintaining simplicity and correctness.