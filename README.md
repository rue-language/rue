# rue

> [!CAUTION]
> Listen, this repo is just for fun. I had it private, but I care more about
> being able to run GitHub Actions to make sure that things are good, so I'm
> open sourcing this repo. Not everything in here is good, or accurate, or
> anything: I'm just messing around. Feel free to take a look but don't look too
> much into this just yet. Someday I'll actually talk about this.

A programming language that starts as a minimal subset of Rust, designed to
explore cutting-edge compiler implementation techniques.

## About

Rue is an experimental programming language with Rust-like syntax that compiles
to native code. It serves as a testbed for modern compiler implementation
techniques while maintaining a simple, understandable codebase.

Key architectural decisions:
- **Incremental compilation** via Salsa for efficient recompilation
- **IDE-first design** with concrete syntax trees preserving all source details
- **Self-contained toolchain** generating ELF executables without external dependencies
- **Multi-stage pipeline**: Lexer → Parser → Semantic Analysis → IR → x86-64 → ELF

The compiler supports both Cargo and Buck2 build systems.

## Current Status

Rue is a fully functional compiler with a complete implementation pipeline:
- **Lexer**: Full tokenization with span tracking
- **Parser**: CST-based parser preserving all syntax details
- **Semantic Analysis**: Type checking and validation
- **Code Generation**: Direct x86-64 assembly via custom IR
- **Executable Output**: Native ELF binaries (no external linker)
- **Runtime**: Built-in functions for I/O, program control, and error handling
- **IDE Support**: LSP server with diagnostics and VS Code extension
- **Build Systems**: Both Cargo and Buck2 supported

**Platform Support**: Linux x86-64 only (generates ELF executables)

## Language Features

Current language support:
- **Variables**: Let bindings with explicit type annotations
- **Assignment**: Reassignment of variables after declaration
- **Arithmetic**: Integer operations (+, -, *, /, %)
- **Comparison**: Relational operators (<=, >=, <, >, ==, !=)
- **Control flow**: if/else expressions and while loops
- **Functions**: Named functions with parameters and return types
- **Type system**: Static typing with mandatory annotations
- **Supported types**: i32, i64, bool, and unit (())
- **Comments**: Single-line (//) and nested multi-line (/* */)
- **Runtime Error Handling**: Division by zero (exit code 250), stack overflow (exit code 251)
- **Expressions**: Everything is an expression (including if/while)
- **I/O functions**: println_i64, println_i32, println_bool, println_unit, input
- **Program control**: exit function for controlled termination

### Example Program

```rue
// Calculate factorial using recursion
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

// Calculate sum of numbers from 1 to n using a while loop
fn sum_to_n(n: i32) -> i32 {
    let sum: i32 = 0;
    let i: i32 = 1;
    
    while i <= n {
        sum = sum + i;
        i = i + 1;
    };
    
    sum
}

/* The main function returns an exit code
   - On Linux, you can see it with: echo $?
   - Exit codes are limited to 0-255 */
fn main() -> i32 {
    let result: i32 = factorial(5);  // 120
    let sum: i32 = sum_to_n(10);     // 55
    
    if result > sum {
        result - sum  // Returns 65 as exit code
    } else {
        0
    }
}
```

### I/O Example

```rue
fn main() -> i64 {
    // Print various types
    println_i64(42);
    println_bool(true);
    println_unit(());  // Prints "()"
    
    // Read input from user
    println_i64(999);  // Prompt
    let x: i64 = input();
    
    // Process and output
    println_i64(x * 2);
    
    // Early exit with custom code
    if x > 100 {
        exit(1);
    };
    
    0
}
```

Running this program:
```bash
$ ./my_program
42
true
()
999
10    # User input
20    # Output: 10 * 2
$ echo $?
0
```

### Type Annotations

Rue requires explicit type annotations for all variable declarations and function signatures:

```rue
// Variable declarations
let x: i32 = 42;
let y: i64 = 1000;
let flag: bool = true;

// Function signatures
fn add(a: i32, b: i32) -> i32 {
    a + b
}

// Functions returning unit type
fn print_value(x: i32) -> () {
    // ... implementation ...
}

// Functions can omit -> () for unit return
fn do_something(x: i32) {
    // Implicitly returns unit type
}
```

Numeric literals default to `i32` unless the context requires a different type.

## Building and Running

### Compile Rue programs

```bash
# Compile a program to executable
cargo run -p rue samples/simple.rue

# Run the compiled program (executable created in same directory as source)
./samples/simple; echo $?  # Shows the program's return value

# Generate assembly instead of executable
cargo run -p rue samples/simple.rue -- --emit-asm
# This creates samples/simple.s with x86-64 assembly

# Specify output file
cargo run -p rue samples/simple.rue -- -o myprogram
cargo run -p rue samples/simple.rue -- --emit-asm -o myprogram.s

# Try other example programs
cargo run -p rue samples/factorial.rue && ./samples/factorial; echo $?
cargo run -p rue samples/fibonacci.rue && ./samples/fibonacci; echo $?
cargo run -p rue samples/while_demo.rue && ./samples/while_demo; echo $?
cargo run -p rue examples/comments.rue && ./examples/comments; echo $?
cargo run -p rue samples/countdown.rue && ./samples/countdown; echo $?
cargo run -p rue samples/assignment_demo.rue && ./samples/assignment_demo; echo $?
```

### With Buck2

```bash
# Compile to executable
buck2 run //crates/rue:rue samples/simple.rue
./samples/simple; echo $?

# Generate assembly
buck2 run //crates/rue:rue samples/simple.rue -- --emit-asm
# Creates samples/simple.s
```

### Running Tests

```bash
# With Cargo
cargo test                    # All tests
cargo test -p rue-lexer       # Lexer tests
cargo test -p rue-parser      # Parser tests
cargo test -p rue-semantic    # Type checking tests
cargo test -p rue-codegen     # Code generation tests
cargo test -p rue             # Integration and type system tests

# With Buck2
buck2 test //crates/...       # All tests
buck2 test //crates/rue-lexer:test    # Lexer tests
buck2 test //crates/rue-parser:test   # Parser tests
buck2 test //crates/rue-semantic:test # Type checking tests
buck2 test //crates/rue-codegen:test  # Code generation tests
buck2 test //crates/rue:integration_tests # Integration tests
buck2 test //crates/rue:type_system_tests # Type system tests
buck2 test //crates/rue:              # All rue tests (integration + type system)
```

## IDE Support

Rue includes a Language Server Protocol (LSP) implementation providing:
- Syntax highlighting (including comments)
- Real-time error diagnostics
- Type checking feedback
- Semantic analysis
- Code completion for built-in functions and keywords
- Hover information with function signatures

```bash
# Install VS Code extension
./install-extension.sh

# Start the language server for other editors
cargo run -p rue-lsp          # With Cargo
buck2 run //crates/rue-lsp    # With Buck2
```

The VS Code extension provides:
- Syntax highlighting for `.rue` files
- Real-time diagnostics as you type
- Comment highlighting (single-line and multi-line)
- Automatic error reporting with precise locations
- Auto-completion for built-in functions (exit, println_i64, println_i32, println_bool, println_unit, input)
- Hover documentation for function signatures

See `crates/rue-lsp/README.md` for editor integration details.

## Development

See [CONTRIBUTING.md](./CONTRIBUTING.md) for development guidance and
[docs/spec.md](./docs/spec.md) for the complete language specification.

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0) MIT license
* ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.