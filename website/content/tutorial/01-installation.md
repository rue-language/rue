+++
title = "Installation"
weight = 1
template = "tutorial/page.html"
+++

# Installation

Rue is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/rue-language/rue/issues)!

## Prerequisites

- [dotslash](https://dotslash-cli.com) - Used to bootstrap Buck2 and the Rust toolchain
- `clang` - Used as the linker (install via your system package manager)

The repository includes everything else: Buck2 is bootstrapped via dotslash, and a hermetic Rust toolchain is downloaded automatically on first build.

## Building from Source

```bash
git clone https://github.com/rue-language/rue
cd rue
./buck2 build //crates/rue:rue
```

The first build will download the Rust toolchain, which may take a minute. Subsequent builds are fast.

That's it! You now have a working Rue compiler. In the next chapter, we'll write our first program.
