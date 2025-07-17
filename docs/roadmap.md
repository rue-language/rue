# Rue Language Roadmap

## Current Status (v0.1.0-dev)

**Note**: Most v0.1.0 planned features are now complete. The remaining work involves local variable scoping improvements and basic I/O operations before the v0.1.0 release.

✅ **Completed Features**:
- 64-bit integer variables with `let` declarations
- Variable assignment and reassignment
- Arithmetic operations (+, -, *, /, %)
- Comparison operations (<=, >=, <, >, ==, !=)
- Control flow: if/else statements and while loops
- Functions with single parameter and return value
- Native x86-64 compilation to ELF executables
- Language Server Protocol (LSP) support with semantic analysis
- VS Code extension with syntax highlighting
- Comments: Single-line `//` and multi-line nested `/* */` comments
- Multiple function parameters with type annotations
- Type system with multiple data types (i32, i64, bool, unit)
- Type annotations for variables and functions
- Type checking and inference for literals
- Improved error messages with clear diagnostics
- Boolean literals: `true` and `false` keywords

## Near-Term Goals (v0.1.0)

### Language Features
- **Local variable scoping improvements**: Better nested scope handling

### Standard Library Expansion
- **I/O operations**: Basic print/input functionality
- **Mathematical functions**: abs, min, max, etc.
- **String literals**: Basic string support (UTF-8)

### Tooling Improvements
- **Formatter**: Automatic code formatting (`rue fmt`)
- **Documentation generator**: Extract docs from comments

## Medium-Term Goals (v0.4-0.6)

### Type System Evolution
- **Additional integer types**: 
  - Signed: i8, i16
  - Unsigned: u8, u16, u32, u64
- **Floating point**: f32, f64
- **Strings**: String type
- **Arrays**: Fixed-size and dynamic arrays

### Advanced Language Features
- **Structs**: User-defined data types
- **Enums**: Algebraic data types
- **Pattern matching**: match expressions
- **Closures**: Anonymous functions with capture
- **Modules**: Code organization and namespacing

### Control Flow Extensions
- **for loops**: Iterator-based iteration
- **loop/break/continue**: Infinite loops with early exit
- **return statements**: Early function return

### Tooling improvements
- **Package manager**: Basic dependency management

## Long-Term Vision (v1.0+)

### Advanced Type System
- **Generics**: Parameterized types and functions
- **Traits**: Interface-like behavior definition
- **Lifetime system**: Memory safety without garbage collection
- **Ownership system**: Rust-like memory management

### Concurrency
- **Async/await**: Asynchronous programming support
- **Channels**: Message passing between tasks
- **Threads**: Low-level threading primitives

### Advanced Features
- **Unsafe code**: Low-level system programming
- **Foreign Function Interface (FFI)**: C library integration

## Compiler Infrastructure Roadmap

### Performance Optimization
- **SSA-based IR**: Static Single Assignment intermediate representation
- **Optimization passes**: Dead code elimination, constant folding, etc.
- **Register allocation**: Efficient CPU register usage
- **LLVM backend**: Alternative high-performance backend

### Platform Support
- **Windows support**: PE executable generation
- **macOS support**: Mach-O executable generation
- **ARM64 support**: Apple Silicon and server ARM
- **WebAssembly target**: Browser and serverless deployment

### Development Experience
- **Incremental compilation**: File-level and module-level caching
- **Parallel compilation**: Multi-threaded compilation pipeline
- **IDE improvements**: Better autocomplete, refactoring, debugging

### Debug and Profiling
- **DWARF debug info**: GDB/LLDB integration
- **Built-in profiler**: Performance analysis tools
- **Memory debugging**: Leak detection and usage analysis
- **Trace generation**: Execution flow visualization

## Research and Exploration Areas

### Cutting-Edge Compiler Techniques
- **Query-based compilation**: Further Salsa integration
- **Persistent data structures**: Immutable AST representations
- **Parallel semantic analysis**: Multi-threaded type checking
- **Incremental linking**: Fast executable regeneration

### Language Design Experiments
- **Linear types**: Resource management through the type system

### Integration Experiments
- **Build system integration**: Treating functions as build targets
- **Hot reloading**: Live code updates during development
- **IDE-compiler fusion**: Deeper editor integration

## Timeline Estimates

- **v0.2**: Local scoping improvements, basic I/O (1-2 months)
- **v0.3**: Additional integer types, arrays, formatter (2-3 months)
- **v0.4**: Structs, enums, pattern matching (4-6 months)
- **v0.5**: Generics, traits, advanced features (6-8 months)
- **v1.0**: Ownership system, full language (12-18 months)

## Success Metrics

### Language Adoption
- **Not being pursued at this time** because this is an experiment

### Technical Excellence
- **Compilation speed**: Sub-second compilation for medium projects
- **Runtime performance**: Within 10% of equivalent C/Rust code
- **Memory safety**: Zero-cost abstractions with safety guarantees
- **IDE experience**: Best-in-class development environment

## Open Questions

### Language Design
- Should Rue have affine or linear types?
- What memory model should concurrent Rue use?

### Implementation Strategy  
- How to balance compilation speed vs. runtime performance?
- How to maintain incremental compilation with advanced features?

---

This roadmap is a living document and will evolve based on user feedback, technical discoveries, and changing priorities. The goal is to build a language that combines the performance of systems languages with the ergonomics of modern high-level languages.