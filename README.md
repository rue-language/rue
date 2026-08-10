# Rue

Rue is an experimental systems programming language exploring memory safety
without garbage collection and higher-level ergonomics than Rust or Zig. It is
implemented in Rust and emits native code directly, without LLVM.

```rue
fn fib(n: i32) -> i32 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() -> i32 {
    println("fib(10) = " + @to_string(fib(10)));
    0
}
```

> [!CAUTION]
> Rue is an early-stage language. Its syntax, semantics, runtime, and standard
> library are still changing, and it is not ready for production use.

## What is here

The repository contains a complete native compiler pipeline, including:

- lexer, parser, semantic analysis, and several inspectable IRs;
- affine ownership, borrowing modes, destructors, structs, enums, arrays,
  strings, modules, comptime, and unsafe/raw-pointer support;
- x86-64 Linux, AArch64 Linux, and AArch64 macOS code generation;
- a small runtime, ELF/Mach-O object and linking support, and direct
  machine-code emitters;
- specification, UI, CLI, differential-oracle, fuzz, sanitizer, and stress
  suites.

The authoritative language definition is the [Rue specification](docs/spec/).
The [tutorial](website/content/tutorial/) is the best introduction to writing
Rue programs. Architecture and design rationale live in
[docs/architecture.md](docs/architecture.md) and [docs/designs/](docs/designs/).

## Build and try Rue

Rue uses Buck2 rather than Cargo. Install
[Dotslash](https://dotslash-cli.com/); Buck2 and the Rust toolchain are then
bootstrapped hermetically by the repository.

```bash
scripts/rue build
scripts/rue exec examples/welcome.rue   # prints 1, 2, 3, 42 and exits 0
scripts/rue test
```

`examples/welcome.rue` is the setup health check: it prints recognizable output
and exits 0, so a successful run is not mistaken for a failure under `set -e` or
CI. (Programs like `examples/fibonacci.rue` return a value from `main`, which
becomes the process exit code — informative, but a nonzero status.)

Use `scripts/rue-bin` when you need the compiler path, or run the real CLI
through the wrapper:

```bash
RUE="$(scripts/rue-bin)"
"$RUE" --help
"$RUE" examples/welcome.rue -o welcome
./welcome
```

Supported targets are `x86-64-linux`, `aarch64-linux`, and `aarch64-macos`.
Cross-target machine-code and assembly emission is available, but producing an
executable may require target-specific system tools.

See [CONTRIBUTING.md](CONTRIBUTING.md) and
[docs/development.md](docs/development.md) for the development workflow.

## Project status

Rue is developed in public as both a language-design project and an experiment
in agent-assisted compiler engineering. Accepted features may remain behind
`--preview` until their implementation and specification are complete. The
test suite tracks normative specification coverage and known implementation
gaps explicitly.

## License

Rue is dual-licensed under the
[Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your
option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in Rue is dual-licensed on the same terms.
