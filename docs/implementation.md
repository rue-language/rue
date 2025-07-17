# Rue Compiler Implementation

## Overview

The Rue compiler is written in Rust and implements a complete compilation pipeline from source code to native x86-64 ELF executables. This document describes the implementation architecture and design decisions.

## Supported Language Features

### Types
- **Primitive types**: `i32`, `i64`, `bool`, `()` (unit)
- **Type annotations**: Required for all variables and function parameters
- **Type inference**: Context-sensitive for literals

### Expressions
- **Arithmetic**: `+`, `-`, `*`, `/`, `%`
- **Comparison**: `<`, `<=`, `>`, `>=`, `==`, `!=`
- **Boolean literals**: `true`, `false`
- **If expressions**: `if condition { expr } else { expr }`
- **While expressions**: `while condition { expr }`
- **Function calls**: `function(arg1, arg2, ...)`

### Statements
- **Variable declarations**: `let name: Type = value`
- **Assignments**: `variable = expression`
- **Expression statements**: Any expression followed by `;`

### Functions
- **Function declarations**: `fn name(param: Type, ...) -> ReturnType { body }`
- **Multiple parameters**: Full support with type annotations
- **Main function**: Flexible return types with appropriate exit codes

### Comments
- **Single-line**: `// comment`
- **Multi-line**: `/* comment */` with nesting support

## Compilation Pipeline

The compiler follows this pipeline:
**Lexer** → **Parser** → **Semantic Analysis** → **TargetIR Generation** → **Code Generation** → **ELF Generation**

The TargetIR (Target Intermediate Representation) layer provides platform-independent code generation and enables future support for multiple backends.

## Implementation Language & Build System

- **Language**: Rust
- **Build System**: Buck2 (with Cargo support for LSP and some tests)
- **Platform Support**: Linux x86-64 only (generates ELF executables)

## Crate Architecture

The Rue compiler is organized into multiple crates for modularity and clean separation of concerns:

- **`rue`**: Main CLI binary and integration tests
- **`rue-lexer`**: Tokenization and lexical analysis
- **`rue-ast`**: Abstract syntax tree definitions
- **`rue-parser`**: Parsing source code into CST
- **`rue-semantic`**: Type checking and semantic analysis
- **`rue-codegen`**: TargetIR generation and x86-64 code emission
- **`rue-compiler`**: High-level compiler API orchestrating all phases
- **`rue-lsp`**: Language Server Protocol implementation

## Architecture Components

### Lexer (`rue-lexer`)
- Hand-written lexical analyzer
- Converts source text into tokens
- Preserves source location information for error reporting
- Support for single-line (`//`) and multi-line (`/* */`) comments with nesting

### Parser (`rue-parser`)
- Hand-written recursive descent parser
- Produces IDE-friendly concrete syntax tree (CST)
- Preserves all tokens and whitespace for LSP features
- Error recovery for better IDE experience
- Support for all language features: functions with multiple typed parameters, let statements, if/else expressions, while loops, assignments, binary operators with proper precedence

### Abstract Syntax Tree (`rue-ast`)

#### Design Philosophy
- **Flat AST**: Inspired by Roslyn's red-green trees and ECS architecture
- **Integer indices**: Instead of pointers for smaller memory footprint and better cache locality
- **Separate arrays**: Different node types stored in separate arrays (ECS-inspired)
- **Generational indices**: Safe node management without lifetime complexity
- **String interning**: All identifiers are interned for memory efficiency

#### Structure
- Nodes are referenced by typed indices rather than pointers
- Each AST contains separate vectors for different node types
- Enables efficient bulk operations and memory layout control

### Semantic Analysis (`rue-semantic`)

#### Incremental Compilation
- **Query-based architecture**: Uses Salsa for incremental computation
- **Expression-level granularity**: Recomputes only changed expressions
- **IDE-first design**: Optimized for interactive development
- Similar to rust-analyzer's incremental approach

#### Type System
- **Multiple primitive types**: i32, i64, bool, and unit (())
- **Explicit type annotations**: Required for all variable declarations and function parameters
- **Type inference**: Automatic type deduction for literals (defaults to i32 for numbers)
- **Context-sensitive literals**: Numeric literals adapt to expected type (e.g., `let x: i64 = 42` works)
- **No implicit conversions**: Strict type checking with clear error messages
- **Error recovery**: Continues analysis even with type errors for better IDE experience

#### Analysis Phases
1. **Name Resolution**: Resolve all identifiers to their declarations
2. **Type Checking**: Verify all expressions are properly typed with explicit annotations
3. **Scope Analysis**: Validate variable scoping rules
4. **Call Graph**: Build function dependency graph
5. **Type Inference**: Deduce types for expressions from context

### TargetIR Generation (`rue-codegen`)

#### Intermediate Representation
- **Platform-independent**: TargetIR abstracts away x86-64 specifics
- **Virtual registers**: Unlimited VReg type for values
- **Type-aware**: Instructions carry type information
- **SSA-friendly**: Design supports future SSA conversion
- **Instruction set**: Copy, BinaryOp, Call, Jump, ConditionalJump, Return, Push, Pop, EnterFrame, LeaveFrame

#### Benefits
- **Multiple backends**: Foundation for LLVM, Cranelift, or other backends
- **Optimization passes**: Can implement optimizations on TargetIR
- **Debugging**: Easier to debug at TargetIR level than raw assembly
- **Testing**: Can test code generation independently from machine code emission

### Code Generation (`rue-codegen`)

#### Strategy
- **Register allocation**: Linear scan allocator with automatic spilling
- **x86-64 target**: Direct native code generation from TargetIR
- **System V ABI**: Compatible with C calling conventions
- **Single-pass assembler**: Direct machine code emission with post-processing fixups

#### Register Allocation
- **Virtual registers**: Unlimited virtual registers mapped to 11 physical registers
- **Physical registers**: RBX, RCX, RDX, RSI, RDI, R8-R15 (RAX reserved for special use)
- **Stack spilling**: Automatic push/pop when registers exhausted
- **LRU eviction**: Least recently used registers are spilled first
- **Smart preservation**: Detects function calls and preserves registers across calls

#### Instruction Generation
- TargetIR instructions lowered to x86-64 machine code
- Type-specific code generation (i32 vs i64 operations)
- Function calls use System V AMD64 ABI (RDI, RSI, RDX, RCX, R8, R9 for parameters)
- Control flow with labels and conditional jumps
- Direct machine code emission (no external assembler)

#### Assembly Process
1. **Code Generation**: Direct emission of machine code bytes
2. **Symbol Collection**: Track labels and forward references
3. **Fixup Pass**: Patch jump targets with resolved addresses

### ELF Generation
- **Direct binary output**: No external linker required
- **Minimal ELF**: Only essential sections (text, data, symbol table)
- **Static linking**: Self-contained executables
- **Linux-specific**: Uses Linux system call ABI

## Design Priorities

1. **Fast compilation speed**: Primary performance goal
2. **Incremental by default**: All computations should be incremental  
3. **IDE-first design**: AST and architecture designed for IDE features
4. **Future extensibility**: Designed to grow into a full language

## Error Handling

### Philosophy
- **Quality first**: Good error messages from the start
- **Source locations**: All errors include precise source positions
- **Incremental friendly**: Errors don't block unrelated analysis
- **IDE integration**: Errors designed for real-time display

### Implementation
- Errors carry source spans for precise highlighting
- Multiple errors can be reported simultaneously
- Recovery strategies maintain partial AST for IDE features

## Testing Strategy

### Unit Tests
- Each compiler phase has comprehensive unit tests
- Property-based testing for core algorithms
- Error condition testing

### Integration Tests
- End-to-end compilation of sample programs
- Executable correctness verification
- Performance regression detection

### Continuous Integration
- Buck2 and Cargo build verification
- Cross-platform testing (when supported)
- Documentation generation and verification

## Development Infrastructure

### Language Server Protocol (LSP)
- Real-time syntax and semantic analysis with full type checking
- IDE integration for VS Code and other editors
- Incremental compilation for responsive experience
- Accurate line/column position reporting via PositionCalculator
- Support for all language features including comments, while loops, and assignments
- Comprehensive error diagnostics including type errors and undefined variables
- VS Code extension with complete syntax highlighting for all comment styles
- **Current limitation**: LSP only works with Cargo due to Buck2 dependency issues

### Debugging Support
- **Current**: GDB integration for compiled programs
- **Future**: DWARF debug information generation
- **Tools**: Built-in disassembly and binary inspection

### Version Control
- Designed for jj (Jujutsu) workflow
- Commit hooks for code quality
- Branching strategy for experimental features

## Build System Integration

### Buck2 Features
- **Incremental builds**: Only rebuild changed components
- **Dependency management**: Reindeer for Cargo.toml → Buck2 conversion
- **Parallel compilation**: Multi-core build execution
- **Target isolation**: Clean separation between compiler and samples

### Reindeer Workflow
1. `reindeer update` - Update Cargo.lock
2. `reindeer vendor` - Vendor dependencies  
3. `reindeer buckify` - Generate Buck2 build files
4. Use fixups/ for problematic dependencies

## Performance Characteristics

### Compilation Speed
- **Target**: Sub-second compilation for small programs
- **Incremental**: Expression-level change detection
- **Memory**: Flat AST reduces allocation overhead
- **I/O**: Minimal file system interaction

### Runtime Performance
- **Code Quality**: Register-allocated code with automatic spilling
- **Binary Size**: Minimal ELF overhead
- **Startup**: Direct native execution, no runtime
- **Memory**: Static allocation only (no heap)
- **Register Usage**: Efficient use of 11 registers with LRU spilling

## Future Architecture Considerations

### Backend Abstraction
- Abstract code generation interface
- Pluggable backends (LLVM, Cranelift)
- Shared optimization passes
- Target-specific lowering

### Multi-Platform Support
- Platform-specific code generation
- Cross-compilation infrastructure  
- ABI compatibility layers
- Binary format abstraction

### Advanced Features
- **Optimization**: SSA-based optimization passes
- **Debug Info**: DWARF generation for debugging
- **Profiling**: Built-in performance profiling
- **Memory Management**: Garbage collection or ownership system