+++
title = "Getting Started"
weight = 2
template = "tutorial/page.html"
+++

# Getting Started

Rue is in early development and there are no binary releases yet. You build the
compiler from source. The repository bootstraps its own build tools, so this is
less work than it sounds.

## Prerequisites

- [Dotslash](https://dotslash-cli.com/), which downloads and runs the pinned
  Buck2 build tool and Rust toolchain.
- `clang`, used as the system linker on platforms where Rue's internal linker
  is not used.

Everything else comes with the repository. The first build downloads what it
needs.

## Building the compiler

```bash
git clone https://github.com/rue-language/rue
cd rue
scripts/rue build
```

The first build takes a few minutes. Later builds are fast.

`scripts/rue` is a small wrapper around the build system. It is the easiest way
to use the compiler while you work through this tutorial, and it works from any
directory inside the repository.

## Running a program

Try the checked-in setup smoke test:

```bash
scripts/rue exec examples/welcome.rue
```

You should see `1`, `2`, `3`, and `42` on separate lines, and the command
should exit with status `0`. `scripts/rue exec` builds the compiler if needed,
compiles the source file to a temporary executable, runs it, and passes the
program's exit status through. That is the whole edit-run loop for the next
several chapters.

## Using the compiler directly

When you want to keep the executable, ask the wrapper for the compiler's path
and tell the compiler where the standard library lives:

```bash
RUE="$(scripts/rue-bin)"
export RUE_STD_PATH="$PWD/std"

"$RUE" examples/welcome.rue -o welcome
./welcome
```

Both lines of setup matter. `scripts/rue-bin` prints the absolute path of the
compiler binary. `RUE_STD_PATH` points the compiler at the repository's `std`
directory; `scripts/rue exec` sets it for you, but a direct invocation does
not, and a program that imports the standard library fails without it:

```text
error: [E0705]: standard library not found
  = help: set RUE_STD_PATH to the toolchain's standard-library directory
```

The rest of this tutorial assumes `RUE` and `RUE_STD_PATH` are set whenever it
shows a direct compiler command.

## Running tests

Rue has a built-in test runner. Once you have a file with tests in it (chapter
13), run them with:

```bash
"$RUE" test main.rue
```

## Targets

Rue produces native executables for `x86-64-linux`, `aarch64-linux`, and
`aarch64-macos`. Cross-target machine-code emission is available, but producing
a runnable executable for another platform may require that platform's system
tools.

## If you work on the compiler

Most people should stay with `scripts/rue`. If you are changing the compiler
itself, you will also meet the lower-level Buck2 commands, for example
`./buck2 build root//crates/rue:rue`. `docs/development.md` in the repository
covers that workflow.

That's it. In the next chapter you'll write your first program.
