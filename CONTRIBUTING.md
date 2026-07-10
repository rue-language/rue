# Contributing to Rue

Rue is early-stage software: language behavior and internal interfaces can
change quickly. Discuss substantial language or architecture changes before
implementing them, and keep normative behavior synchronized with the
specification and its tests.

## Setup

Install [Dotslash](https://dotslash-cli.com/), clone the repository, then use
the repository wrappers:

```bash
scripts/rue build
scripts/rue exec examples/welcome.rue   # prints 1, 2, 3, 42 and exits 0
scripts/rue quick
scripts/rue test
```

The wrappers bootstrap Buck2 and a hermetic Rust toolchain. macOS also requires
Xcode Command Line Tools for linking Rue executables.

## Before submitting a change

```bash
scripts/rue fmt
scripts/rue test
```

Use a targeted spec, UI, or CLI test while iterating. Language behavior belongs
in specification tests; diagnostic presentation belongs in UI tests; driver,
ABI, multi-file, and runtime-I/O behavior belongs in CLI tests. New language
features must be preview-gated until complete.

Rue uses Jujutsu (`jj`) for local version control and Linear (`RUE-NN`) for
issue tracking. Do not commit directly on `trunk`.

## Documentation

- [Development guide](docs/development.md)
- [Compiler architecture](docs/architecture.md)
- [Language overview](docs/language.md)
- [Language specification](docs/spec/)
- [Architecture decision records](docs/designs/)
- [Project processes](docs/process/)

Open a GitHub issue for public bug reports and discussion.
