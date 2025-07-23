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

### Built-in Functions
- **I/O operations**: `println_i64()`, `println_i32()`, `println_bool()`, `println_unit()`, `input()`
- **Program control**: `exit()`

## Compilation Pipeline

The compiler follows this pipeline:
**Lexer** → **Parser** → **Semantic Analysis** → **HIR Generation** → **Instruction Generation** → **x86-64 Emission** → **ELF Generation**

The compilation uses a multi-level intermediate representation:
- **HIR (High-level IR)**: Typed, desugared representation after semantic analysis (in `rue-ir`)
- **Instruction enum**: Platform-independent instructions with virtual registers (in `rue-codegen`)  
- **MachineInstr**: Low-level x86-64 instructions with physical registers (in `rue-ir`)

The HIR serves as a clean abstraction layer between semantic analysis and code generation, containing only essential semantic information with all syntactic details removed.

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
- **`rue-ir`**: High-level IR (HIR) definitions and target-specific representations (MachineInstr for x86-64)
- **`rue-codegen`**: Code generation using IR definitions from rue-ir
- **`rue-runtime`**: Runtime library embedded in executables
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
6. **HIR Generation**: Lower typed AST to simplified intermediate representation

#### Variable Scoping Implementation

The semantic analyzer implements block-level scoping using a hierarchical scope stack:

**ScopeStack Structure**:
- Maintains a stack of HashMap<String, RueType> for variable lookups
- Each block (function body, if/else branches, while loops) creates a new scope
- Variable declarations add to the innermost scope
- Variable lookups search from innermost to outermost scope

**Scope Management**:
1. **Function entry**: Creates new scope, adds parameters
2. **Block entry** (if/else/while): Pushes new scope onto stack
3. **Block exit**: Pops scope from stack
4. **Variable declaration**: Adds to current (innermost) scope
5. **Variable reference**: Searches scopes from inner to outer

**Variable Shadowing**:
- Variables in inner scopes can shadow outer scope variables
- Each scope maintains its own variable-to-type mapping
- Shadowed variables remain accessible when inner scope exits

The code generator mirrors this structure with a VarScopeStack that maps variables to virtual registers (VRegs), ensuring consistent scoping behavior throughout compilation.

### Intermediate Representations (`rue-ir` and `rue-codegen`)

#### Overview
The compiler uses intermediate representations at three levels:
- **HIR (in `rue-ir`)**: High-level, language-specific representation after semantic analysis
- **Instruction enum (in `rue-codegen`)**: Platform-independent instructions with virtual registers
- **MachineInstr (in `rue-ir`)**: Low-level x86-64 machine instructions with physical registers

#### High-level IR: HIR (Language-Specific)
Located in `rue-ir/src/hir.rs`, HIR is the typed, desugared representation after semantic analysis:
- **Type preservation**: All expressions carry complete type information from semantic analysis
- **Desugared syntax**: Removes syntactic details like parentheses, semicolons, and brackets
- **Structured representation**: Functions, blocks, statements, and expressions with clear hierarchy
- **Source tracking**: Maintains source location information for error reporting
- **Core constructs**: HirProgram, HirFunction, HirBlock, HirStatement, HirExpr with variants for all language features
- **Typed literals**: HirLiteral enum with concrete types (Int32, Int64, Bool, Unit)
- **Control flow**: Native representation of if expressions and while loops
- **Function calls**: Direct representation with typed arguments and return types

HIR Benefits:
- **Clean abstraction**: Separates semantic analysis concerns from code generation
- **Future extensibility**: Enables optimization passes between semantic analysis and codegen
- **Better testing**: HIR can be tested independently with round-trip validation
- **Code clarity**: Simpler code generation logic working with desugared representation

#### HIR Examples

Here's how Rue source code is transformed to HIR:

**Source Code:**
```rue
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
```

**HIR Representation:**
```
HirFunction {
  name: "factorial",
  params: [("n", I32)],
  return_type: I32,
  body: HirBlock {
    statements: [],
    expr: Some(HirExpr::If {
      cond: HirExpr::Binary {
        op: Le,
        lhs: HirExpr::Var { name: "n", ty: I32 },
        rhs: HirExpr::Literal { lit: Int32(1) },
        ty: Bool
      },
      then_block: HirBlock {
        statements: [],
        expr: Some(HirExpr::Literal { lit: Int32(1) })
      },
      else_block: Some(HirBlock {
        statements: [],
        expr: Some(HirExpr::Binary {
          op: Mul,
          lhs: HirExpr::Var { name: "n", ty: I32 },
          rhs: HirExpr::Call {
            func: "factorial",
            args: [HirExpr::Binary {
              op: Sub,
              lhs: HirExpr::Var { name: "n", ty: I32 },
              rhs: HirExpr::Literal { lit: Int32(1) },
              ty: I32
            }],
            ty: I32
          },
          ty: I32
        })
      }),
      ty: I32
    })
  }
}
```

**Key Transformations:**
- All syntax tokens (braces, semicolons, parentheses) removed
- Type information attached to every expression
- Operator precedence made explicit through tree structure
- Block structure represented explicitly with HirBlock
- Source locations preserved but not shown here for clarity

#### Platform-Independent IR: Instruction enum
Located in `rue-codegen`, this serves as the platform-independent IR:
- **Virtual registers**: Unlimited VReg type for values, allocated later
- **Type-aware**: Instructions carry type information implicitly
- **SSA-friendly**: Design supports future SSA conversion
- **Instruction set**: Copy, BinaryOp, Call, Jump, Branch, Return, Push, Pop, Load, Store, Syscall, EnterFrame, LeaveFrame
- **Control flow**: Labels and conditional branches for structured control

#### Low-Level IR: MachineInstr (x86-64 Specific)
Located in `rue-ir::target`, this represents actual x86-64 instructions:
- **Physical registers**: Direct mapping to x86-64 registers (RAX, RBX, etc.)
- **Machine opcodes**: MovRR, MovRI32, MovRI64, AddRR, SubRR, ImulRR, Idiv, CmpRR, SetCC, Push, Pop, Call, Ret, Jmp, JmpCC, Syscall
- **Memory operations**: Stack-relative addressing with RBP offsets
- **Direct encoding**: Each instruction maps to specific x86-64 opcodes

#### Architecture Note
While the original design documents refer to "TargetIR", the actual implementation evolved differently. The `Instruction` enum in `rue-codegen` serves as the high-level IR, and `MachineInstr` in `rue-ir::target` serves as the low-level, target-specific IR. This provides the same benefits of separation but with a simpler implementation.

#### Benefits
- **Clean separation**: Platform-independent and platform-specific code clearly separated
- **Multiple backends**: The Instruction enum can be lowered to different architectures
- **Optimization passes**: Can implement optimizations at either IR level
- **Testing**: Each IR level can be tested independently

### Code Generation (`rue-codegen`)

#### Strategy  
- **Three-phase generation**: HIR → Instruction enum → MachineInstr → machine code bytes
- **Register allocation**: Linear scan allocator with automatic spilling
- **x86-64 target**: Uses MachineInstr definitions from rue-ir
- **System V ABI**: Compatible with C calling conventions
- **Single-pass assembler**: Direct machine code emission with post-processing fixups

#### Register Allocation
- **Virtual registers**: Unlimited virtual registers mapped to 11 physical registers
- **Physical registers**: RBX, RCX, RDX, RSI, RDI, R8-R15 (RAX reserved for special use)
- **Stack spilling**: Automatic push/pop when registers exhausted
- **LRU eviction**: Least recently used registers are spilled first
- **Smart preservation**: Detects function calls and preserves registers across calls

#### Instruction Generation
- HIR expressions generate platform-independent Instructions with virtual registers
- Instructions are then lowered to MachineInstr with physical registers
- MachineInstr emitted as x86-64 machine code bytes
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

### Runtime System (`rue-runtime`)

#### Architecture
The runtime is embedded directly into each compiled executable, providing essential services without external dependencies.

#### Components
- **Syscall wrappers**: Direct Linux system calls for I/O
- **Built-in functions**: I/O primitives (print, input) and program control (exit)
- **Type conversions**: Integer-to-string and string-to-integer for I/O
- **Error handling**: Division-by-zero detection with controlled termination

#### Implementation Strategy
- **No external dependencies**: Uses only Linux syscalls
- **Minimal footprint**: Adds < 2KB to executable size
- **Direct syscalls**: No libc dependency
- **Machine code generation**: Runtime functions generated using MachineInstr from rue-ir
- **Assembly fallback**: Complex operations use inline assembly strings when needed

#### Built-in Functions
- `exit(code: i64) -> ()`: Terminate with exit code
- `println_i64(value: i64) -> ()`: Print 64-bit integer
- `println_i32(value: i32) -> ()`: Print 32-bit integer  
- `println_bool(value: bool) -> ()`: Print boolean as "true"/"false"
- `println_unit(value: ()) -> ()`: Print "()"
- `input() -> i64`: Read integer from stdin
- `to_i32(value: i64) -> i32`: Truncate 64-bit integer to 32 bits
- `to_i64(value: i32) -> i64`: Sign-extend 32-bit integer to 64 bits

#### Error Codes
- **250**: Division or modulo by zero
- **251**: Stack overflow (planned)

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
- Runtime function testing with I/O capture

### Integration Tests
- End-to-end compilation of sample programs
- Executable correctness verification
- Type system tests for all type features
- Runtime tests for built-in functions
- Performance regression detection

### Test Organization
- **Unit tests**: In each crate's `src/test.rs` or inline
- **Integration tests**: In `crates/rue/tests/`
  - `corpus_tests.rs`: Sample program tests
  - `type_system_tests.rs`: Type checking scenarios
  - `runtime_tests.rs`: Built-in function tests
  - `comprehensive_sample_tests.rs`: All sample programs
  - `dual_function_call_tests.rs`: Complex call scenarios

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

## Building the Compiler

### Cargo Build
The standard Rust build system works out of the box:
```bash
# Build the compiler
cargo build

# Run the compiler
cargo run -p rue samples/simple.rue

# Run all tests
cargo test

# Run specific test suite
cargo test -p rue-lexer
cargo test -p rue-parser
cargo test -p rue-semantic
cargo test -p rue-codegen
cargo test -p rue-compiler
cargo test -p rue-runtime
cargo test -p rue
```

### Buck2 Build
Buck2 provides faster incremental builds:
```bash
# Build the compiler
buck2 build //crates/rue:rue

# Run the compiler
buck2 run //crates/rue:rue samples/simple.rue

# Run all tests
buck2 test //crates/...

# Run specific test suite
buck2 test //crates/rue-lexer:test
buck2 test //crates/rue-parser:test
buck2 test //crates/rue-semantic:test
buck2 test //crates/rue-codegen:test
buck2 test //crates/rue-compiler:test
buck2 test //crates/rue-runtime:test
buck2 test //crates/rue:corpus_tests
buck2 test //crates/rue:type_system_tests
```

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