+++
title = "Installation"
weight = 1
template = "tutorial/page.html"
+++

# Installation

Rue is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/rue-language/rue/issues)!

## Prerequisites

- [Dotslash](https://dotslash-cli.com/) - Used to bootstrap Buck2 and
  the Rust toolchain
- `clang` - Used as the system linker on platforms where the internal linker is
  not used

The repository includes everything else. Buck2 and the Rust toolchain are
bootstrapped hermetically, and the first build downloads what it needs.

## Building from Source

```bash
git clone https://github.com/rue-language/rue
cd rue
scripts/rue build
```

The first build may take a minute. Subsequent builds are fast.

## Running an Example

Try one of the checked-in examples:

```bash
scripts/rue exec examples/hello.rue
```

`scripts/rue exec` builds the compiler if needed, compiles the Rue source to a
temporary executable, and runs it.

## Compiling Manually

When you want to keep the executable, ask the wrapper for the compiler path and
run the compiler directly:

```bash
RUE="$(scripts/rue-bin)"
"$RUE" examples/hello.rue -o hello
./hello
```

Rue currently supports native code generation for `x86-64-linux`,
`aarch64-linux`, and `aarch64-macos`. Cross-target assembly and machine-code
emission are available, but producing an executable may require target-specific
system tools.

## Lower-Level Buck2 Commands

Most users should start with `scripts/rue`. If you're working on the compiler
itself, you may also see lower-level Buck2 commands such as:

```bash
./buck2 build root//crates/rue:rue
./buck2 test root//:spec-tests
```

The `./buck2` binary is also bootstrapped by Dotslash.

That's it! You now have a working Rue compiler. In the next chapter, we'll
write our first program.
