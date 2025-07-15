# Code Generation Design Decisions

## Core Architecture Choices

### 1. Three-Stage Pipeline Design

**Decision**: Separate code generation into distinct stages: CodeGen → Assembly → ELF
**Rationale**: 
- Clear separation of concerns
- Easier testing and debugging
- Allows different backends (could add LLVM later)
- Follows traditional compiler architecture

**Alternative Considered**: Direct AST-to-machine-code generation
**Why Rejected**: Too complex, harder to debug, less modular

### 2. Stack-Based Expression Evaluation

**Decision**: Use stack-based evaluation for all expressions
**Rationale**:
- Handles arbitrary expression complexity
- Simple to implement and understand  
- No register allocation complexity
- Matches current language simplicity

**Alternative Considered**: Register-based evaluation with allocation
**Why Rejected**: Too complex for initial implementation, premature optimization

### 3. x86-64 Target Architecture

**Decision**: Target x86-64 exclusively initially
**Rationale**:
- Most common development platform
- Well-documented instruction set
- Good tooling support for debugging
- Matches author's development environment

**Alternative Considered**: LLVM backend for multi-platform support
**Why Rejected**: Adds dependency complexity, harder to understand low-level details

## Instruction Set Design

### 4. Comprehensive Instruction Enum

**Decision**: Create enum covering all needed x86-64 instructions
**Rationale**:
- Type safety prevents instruction encoding errors
- Easy to extend with new instructions
- Clear documentation of supported operations
- Enables instruction-level testing

**Alternative Considered**: String-based assembly generation
**Why Rejected**: Error-prone, harder to validate, less type-safe

### 5. Operand Type System

**Decision**: Separate Register, Immediate, Memory, Label operand types
**Rationale**:
- Matches x86-64 addressing modes
- Enables compile-time validation of operand combinations
- Clear documentation of instruction capabilities
- Simplifies machine code generation

## Assembly and Machine Code Generation

### 6. Two-Pass Assembler Design

**Decision**: First pass for symbol collection, second pass for code emission
**Rationale**:
- Standard assembler design pattern
- Handles forward references naturally
- Separates concerns cleanly
- Enables optimization passes in future

**Alternative Considered**: Single-pass with backpatching
**Why Rejected**: More complex state management, harder to debug

### 7. Manual Instruction Encoding

**Decision**: Hand-code x86-64 instruction encoding
**Rationale**:
- Complete control over generated code
- No external dependencies
- Educational value - understanding machine code
- Enables future optimizations

**Alternative Considered**: Use assembler library or external assembler
**Why Rejected**: Adds dependency, less educational, harder to optimize

### 8. Relocation System Design

**Decision**: Simple 32-bit relative relocations only
**Rationale**:
- Sufficient for current language features
- Simple to implement and debug
- Standard x86-64 approach for local jumps/calls
- Can extend later if needed

## ELF Generation

### 9. Minimal ELF Implementation

**Decision**: Generate minimal but complete ELF executables
**Rationale**:
- Produces real executables that run on Linux
- Educational - understanding executable format
- No external linker dependency
- Complete control over output

**Alternative Considered**: Generate object files and use system linker
**Why Rejected**: More complex build process, less educational

### 10. Single LOAD Segment Design

**Decision**: Use single PT_LOAD segment for code
**Rationale**:
- Simplest possible executable layout
- Sufficient for current language features
- Easy to understand and debug
- Can extend with data segment later

## Calling Convention

### 11. System V AMD64 ABI Compliance

**Decision**: Follow standard System V calling convention
**Rationale**:
- Enables interoperability with C libraries
- Standard approach for x86-64 Linux
- Well-documented and tested
- Future-proofs for library integration

**Alternative Considered**: Custom calling convention
**Why Rejected**: Would prevent library integration, non-standard

### 12. Stack-Based Parameter Storage

**Decision**: Store parameters on stack rather than keeping in registers
**Rationale**:
- Simplifies variable access implementation
- Uniform treatment of parameters and local variables
- Easier to implement initially
- Can optimize later

**Alternative Considered**: Keep parameters in registers when possible
**Why Rejected**: More complex implementation, premature optimization

## Error Handling

### 13. Simple String-Based Error Messages

**Decision**: Use simple CodegenError struct with string messages
**Rationale**:
- Easy to implement and understand
- Sufficient error information for debugging
- Consistent with other compiler phases
- Can enhance with structured errors later

**Alternative Considered**: Structured error types with source locations
**Why Rejected**: More complex to implement, current error reporting sufficient

## Testing Strategy

### 14. Comprehensive Test Suite Design

**Decision**: Test at multiple levels - unit, integration, end-to-end
**Rationale**:
- Catches errors at different abstraction levels
- Enables confident refactoring
- Documents expected behavior
- Critical for low-level code correctness

### 15. Real Example Testing (Factorial)

**Decision**: Use factorial as primary integration test
**Rationale**:
- Tests recursion, arithmetic, control flow
- Complex enough to catch real bugs
- Easy to verify correct output (120)
- Demonstrates real-world capability

## Performance Considerations

### 16. Development Speed Over Runtime Performance

**Decision**: Prioritize implementation simplicity over generated code performance
**Rationale**:
- First goal is working compiler
- Optimization can come later
- Educational value of simple implementation
- Easier to debug and understand

**Alternative Considered**: Optimize generated code from start
**Why Rejected**: Premature optimization, would slow development

### 17. Memory Allocation Strategy

**Decision**: Simple Vec-based instruction storage
**Rationale**:
- Simple and reliable
- Good performance for expected code sizes
- Easy to debug and inspect
- Can optimize memory usage later

## Future Evolution Path

### 18. Extensible Design Patterns

**Decision**: Design for future extension rather than current optimization
**Rationale**:
- Easier to add features incrementally
- Reduces risk of architectural dead ends
- Enables experimentation with new features
- Matches incremental development approach

These design decisions prioritize:
1. **Correctness** over performance
2. **Simplicity** over optimization  
3. **Education** over efficiency
4. **Extensibility** over current completeness

This approach enables rapid development of a working compiler while maintaining clear upgrade paths for future enhancements.