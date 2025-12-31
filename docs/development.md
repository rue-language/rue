# Development Guide

This document covers how to build, test, and contribute to the Rue compiler.

## Prerequisites

### Required

- **dotslash** - For bootstrapping Buck2. Install via `brew install dotslash` (macOS) or see [dotslash docs](https://dotslash-cli.com/).

### Platform-Specific

The Rust toolchain is downloaded automatically by Buck2 (hermetic build). However, running Rue programs has platform requirements:

| Platform | Requirements |
|----------|--------------|
| Linux x86_64 | None (fully hermetic) |
| macOS ARM64 | Xcode Command Line Tools (`xcode-select --install`) |
| macOS x86_64 | Xcode Command Line Tools (`xcode-select --install`) |

**Why macOS needs Xcode**: The Rue compiler uses the system `clang` to link executables on macOS. On Linux, Rue uses its own internal ELF linker.

### Optional (for IDE support)

- **Rust toolchain via rustup** - For IDE features (rust-analyzer, etc.). The `rust-toolchain.toml` in the repo root ensures the right version.

## Repository Structure

```
rue/
├── crates/
│   ├── rue/           # CLI binary
│   ├── rue-air/       # Typed IR + semantic analysis
│   ├── rue-codegen/   # Machine code generation
│   ├── rue-compiler/  # Pipeline orchestration
│   ├── rue-error/     # Error types
│   ├── rue-lexer/     # Tokenizer
│   ├── rue-linker/    # Object file creation and linking
│   ├── rue-parser/    # AST construction
│   ├── rue-rir/       # Untyped IR
│   ├── rue-runtime/    # The runtime
│   ├── rue-span/      # Source locations
│   └── rue-spec/      # Specification test runner
├── docs/              # Documentation
├── examples/          # Example .rue programs
├── third-party/       # Vendored dependencies
└── toolchains/        # Buck2 toolchain definitions
```

## Building

### Build Everything

```bash
./buck2 build //...
```

### Build the Compiler

```bash
./buck2 build //crates/rue:rue
```

The binary is output to `buck-out/v2/gen/root/crates/rue/__rue__/rue`.

### Build a Specific Crate

```bash
./buck2 build //crates/rue-lexer:rue-lexer
```

## Testing

### Run All Tests

```bash
./test.sh
```

This runs:
1. Unit tests for all crates (`buck2 test`)
2. Specification tests (`buck2 run //crates/rue-spec:rue-spec`)

### Run Unit Tests Only

```bash
./buck2 test //...
```

### Run Spec Tests Only

```bash
./buck2 run //crates/rue-spec:rue-spec
```

### Run a Specific Test

```bash
./buck2 test //crates/rue-lexer:rue-lexer-test
```

### Filter Spec Tests

```bash
./buck2 run //crates/rue-spec:rue-spec -- "1.1"  # Run section 1.1 tests
./buck2 run //crates/rue-spec:rue-spec -- "zero" # Run tests matching "zero"
```

## Using the Compiler

### Compile a Program

```bash
./buck2 run //crates/rue:rue -- source.rue output
./output
echo $?  # Check exit code
```

### Dump Intermediate Representations

```bash
# Dump RIR (untyped IR)
./buck2 run //crates/rue:rue -- --dump-rir source.rue

# Dump AIR (typed IR)
./buck2 run //crates/rue:rue -- --dump-air source.rue

# Dump MIR (machine IR before register allocation)
./buck2 run //crates/rue:rue -- --dump-mir source.rue
```

## Adding Tests

### Unit Tests

Add tests to the relevant crate's source file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // ...
    }
}
```

Ensure the crate has a `rust_test` target in its `BUCK` file.

### Specification Tests

Add test cases to `.toml` files in `crates/rue-spec/cases/`:

```toml
[[case]]
name = "my_test"
source = "fn main() -> i32 { 42 }"
exit_code = 42
```

For compile-fail tests:

```toml
[[case]]
name = "my_error_test"
source = "fn main() { }"
compile_fail = true
error_contains = "expected '->'"
```

For golden tests (exact output matching):

```toml
[[case]]
name = "my_golden_test"
source = "fn main() -> i32 { 42 }"
expected_air = """
function main:
air (return_type: i32) {
    %0 : i32 = const 42
    %1 : i32 = ret %0
}
"""
```

## Version Control

This project uses [Jujutsu (jj)](https://github.com/martinvonz/jj) for version control.

### Common Commands

```bash
jj status          # Show working copy changes
jj diff            # Show diff
jj commit -m "msg" # Create a commit
jj log             # Show history
```

### Creating a Commit

```bash
jj commit -m "Add feature X"
```

## Code Style

- Follow standard Rust formatting (`rustfmt`)
- Keep functions small and focused
- Prefer explicit types in public APIs
- Add doc comments to public items

## Debugging

### Print IR at Each Stage

Use the `--dump-*` flags to see the IR at each compilation stage:

```bash
echo 'fn main() -> i32 { 42 }' > /tmp/test.rue
./buck2 run //crates/rue:rue -- --dump-rir /tmp/test.rue
./buck2 run //crates/rue:rue -- --dump-air /tmp/test.rue
./buck2 run //crates/rue:rue -- --dump-mir /tmp/test.rue
```

### Disassemble Output

```bash
./buck2 run //crates/rue:rue -- /tmp/test.rue /tmp/test
objdump -d /tmp/test
```

### Run Under GDB

```bash
./buck2 run //crates/rue:rue -- /tmp/test.rue /tmp/test
gdb /tmp/test
```

## Common Tasks

### Add a New Crate

1. Create directory `crates/rue-newcrate/`
2. Add `BUCK` file with `rust_library` and `rust_test` targets
3. Add to dependencies in consuming crates' `BUCK` files

### Add a Third-Party Dependency

Dependencies are vendored in `third-party/`. See existing setup for patterns.

### Modify the Grammar

1. Update `rue-lexer` if new tokens needed
2. Update `rue-parser` for new syntax
3. Update `rue-rir` for new IR instructions
4. Update `rue-air` for typed versions
5. Update `rue-codegen` for code generation
6. Add spec tests for new behavior
