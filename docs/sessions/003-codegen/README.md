# Session 3: Code Generation Implementation

**Date**: January 2025  
**Focus**: Complete x86-64 code generation and ELF executable output

## Overview

This session implemented the complete code generation pipeline for the rue programming language, transforming the parsed AST into native x86-64 executables. This represents a major milestone - rue can now compile programs to native code that runs directly on the target system.

## Major Accomplishments

### 1. Code Generation Architecture
- **Instruction Representation**: Created comprehensive x86-64 instruction enum covering arithmetic, control flow, memory operations, and system calls
- **Register Management**: Implemented proper register allocation using standard x86-64 registers (rax, rbx, rcx, etc.)
- **Stack-based Evaluation**: Used stack-based expression evaluation with proper operand ordering

### 2. Assembly and Machine Code Generation
- **Two-pass Assembler**: Implemented symbol resolution with first pass for address calculation and second pass for code emission
- **Instruction Encoding**: Proper x86-64 machine code generation with REX prefixes, ModR/M bytes, and immediate values
- **Relocation Handling**: Symbol table management and relative address calculation for jumps and calls

### 3. ELF Executable Generation
- **ELF Headers**: Complete ELF64 header generation with proper magic numbers, architecture flags, and entry points
- **Program Headers**: PT_LOAD segment with correct permissions (readable + executable)
- **Memory Layout**: Proper virtual address mapping starting at 0x400000

### 4. Calling Convention
- **System V AMD64 ABI**: Implemented standard calling convention with rdi for first parameter, rax for return values
- **Function Prologue/Epilogue**: Proper stack frame setup with rbp management
- **Parameter Passing**: Stack-based parameter storage with correct offset calculations

## Key Technical Implementations

### Instruction Size Calculation
Fixed critical bug in `instruction_size()` function that was causing segmentation faults:
- **Push/Pop**: Corrected from 2 bytes to 1 byte (opcodes 50+r, 58+r)
- **Mov Instructions**: Made context-sensitive based on operand types (1-10 bytes)
- **Comparison**: Context-sensitive sizing for register vs immediate operands

### Expression Code Generation
- **Binary Operations**: Left-to-right evaluation with stack preservation
- **Function Calls**: Proper argument passing in rdi register
- **Variable Access**: Stack-relative addressing with rbp offsets
- **Literals**: Direct immediate value loading

### Control Flow
- **If/Else Statements**: Label generation with proper jump instructions (je, jmp)
- **Comparison Operations**: Full implementation of <= operator with conditional jumps
- **Recursive Functions**: Proper stack management for recursive calls like factorial

## Testing and Validation

### Comprehensive Test Suite
- **Unit Tests**: Code generation for expressions, statements, and functions
- **Integration Tests**: End-to-end compilation from source to executable
- **Assembler Tests**: Machine code generation and symbol resolution
- **ELF Tests**: Executable format validation

### Real-world Example
Successfully compiled and executed factorial program:
```rust
fn factorial(n) {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() {
    factorial(5)  // Returns 120
}
```

### Debugging Process
- **Segfault Investigation**: Used gdb and hexdump to identify instruction size calculation errors
- **Symbol Resolution**: Fixed relative address calculations for function calls
- **Stack Management**: Ensured proper stack alignment and cleanup

## CI/CD Integration

### Build System Updates
- **GitHub Actions**: Updated CI workflow to compile simple.rue instead of running with no arguments
- **Cross-platform Testing**: Ensured compatibility with both Cargo and Buck2 build systems
- **Integration Testing**: Added executable generation verification

## Code Quality

### Architecture Decisions
- **Separation of Concerns**: Clean separation between code generation, assembly, and ELF generation
- **Error Handling**: Comprehensive error reporting with descriptive messages
- **Documentation**: Inline comments explaining x86-64 instruction encoding details

### Performance Considerations
- **Incremental Compilation**: Built on Salsa framework for future incremental compilation support
- **Memory Efficiency**: Stack-based evaluation minimizes register pressure
- **Code Size**: Compact instruction encoding following x86-64 standards

## Future Enhancements

### Immediate Opportunities
- **Optimization**: Basic peephole optimizations and dead code elimination
- **More Data Types**: Support for strings, arrays, and structured data
- **Standard Library**: Basic I/O operations and utility functions

### Advanced Features
- **Register Allocation**: Move from stack-based to register-based evaluation
- **LLVM Backend**: Alternative backend for better optimization and cross-platform support
- **Debug Information**: DWARF debug info generation for debugging support

## Technical Challenges Overcome

1. **Instruction Encoding Complexity**: Mastered x86-64 variable-length instruction encoding
2. **Symbol Resolution**: Implemented two-pass assembler with proper relocation handling
3. **Stack Management**: Correct stack frame setup and parameter passing
4. **Segfault Debugging**: Systematic approach to debugging low-level assembly issues

## Lessons Learned

- **Low-level Debugging**: Importance of systematic debugging with tools like gdb and hexdump
- **Instruction Documentation**: Critical need for accurate instruction size calculations
- **Test-driven Development**: Comprehensive testing caught multiple edge cases
- **Incremental Implementation**: Building complexity gradually prevented overwhelming bugs

## Session Outcome

The rue compiler now generates working native x86-64 executables from source code. This represents a complete compiler implementation from lexical analysis through code generation to executable output. The factorial example demonstrates that complex recursive programs compile and execute correctly, validating the entire compiler pipeline.

This session transformed rue from a parser-only implementation into a fully functional compiler capable of producing native executables that run directly on x86-64 systems.