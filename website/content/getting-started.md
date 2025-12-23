+++
title = "Getting Started"
+++

## Installation

Rue is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/rue-language/rue/issues)!

### Prerequisites

- Rust toolchain (for building the compiler)
- Buck2 build system

### Building from Source

```bash
git clone https://github.com/rue-language/rue
cd rue
./buck2 build //crates/rue:rue
```

### Your First Program

Create a file called `hello.rue`:

```rust
fn main() -> i32 {
    @dbg(42);
    0
}
```

Compile and run it:

```bash
./buck2 run //crates/rue:rue -- hello.rue hello
./hello
# prints: 42
echo $?  # prints: 0
```

## Next Steps

- Read the [Language Specification](/spec/) for complete documentation
- Check out the [GitHub repository](https://github.com/rue-language/rue) for examples
