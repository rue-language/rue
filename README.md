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
to native code. It focuses on modern compiler architecture including incremental
compilation using Salsa, IDE-first design with concrete syntax trees, and direct
ELF executable generation without external tooling.

The compiler supports both Cargo and Buck2 build systems.

## Current Status

The compiler is fully functional with a complete implementation pipeline from
lexing through native code generation. It includes an LSP server for IDE
integration and VS Code extension.

**Platform Support**: Linux x86-64 only

## Language Features

Current language support:
- Variables and assignment (let statements)
- Arithmetic operations (+, -, *, /, %)
- Control flow (if/else, while loops)
- Functions with optional parameters
- Static type system with type annotations
- Type checking and inference for literals
- Supported types: i32, i64, bool, and unit (())

### Example Program

```rue
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)  // Returns 120 as the exit code
}
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
```

### With Buck2

```bash
buck2 run //crates/rue:rue samples/simple.rue
./samples/simple; echo $?
```

### Running Tests

```bash
# With Cargo
cargo test                    # All tests
cargo test -p rue-lexer       # Just lexer tests
cargo test -p rue-parser      # Just parser tests

# With Buck2
buck2 test //crates/...       # All tests
buck2 test //crates/rue-lexer:test    # Just lexer tests
buck2 test //crates/rue-parser:test   # Just parser tests
```

## IDE Support

The language server provides syntax highlighting and error detection:

```bash
# Install VS Code extension
./install-extension.sh

# Start the language server for other editors
cargo run -p rue-lsp          # With Cargo
buck2 run //crates/rue-lsp    # With Buck2
```

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