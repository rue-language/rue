# Contributing to Rue

Rue is early-stage software: language behavior and internal interfaces can
change quickly. Discuss substantial language or architecture changes before
implementing them, and keep normative behavior synchronized with the
specification and its tests.

This guide is for **external contributors** working through GitHub. You do not
need any special tooling, accounts, or access beyond a GitHub account and a
local clone. (The maintainers run an additional internal workflow — see
[How the maintainers work](#how-the-maintainers-work) — but none of it is
required to contribute.)

## Setup

Clone the repository and install [Dotslash](https://dotslash-cli.com/), then use
the repository wrappers:

```bash
git clone https://github.com/rue-language/rue
cd rue
scripts/rue build                        # build the compiler, print its path
scripts/rue exec examples/welcome.rue    # prints 1, 2, 3, 42 and exits 0
scripts/rue quick                        # fast unit tests (~2-5s)
scripts/rue test                         # full suite (unit + spec + UI + CLI)
```

The wrappers bootstrap Buck2 and a hermetic Rust toolchain; `scripts/rue build`
just forwards to `./buck2 build //crates/rue:rue`, so you can drive Buck2
directly if you prefer. macOS also requires Xcode Command Line Tools for linking
Rue executables.

The repository's own tooling is not hermetic: its gates and test runners are
Python scripts that use whichever `python3` your `PATH` resolves, and a few of
them require Python 3.11 or newer. macOS ships `/usr/bin/python3` as 3.9.6, so
on a stock Mac `scripts/rue premerge` will report that a target needs a newer
interpreter until you install one and put it earlier on `PATH`. See AGENTS.md,
"Repository tooling baseline".

On development machines with several Rue worktrees, unfiltered full suites are
serialized automatically to avoid oversubscribing the host. Quick and filtered
checks remain concurrent. Maintainers can opt into the shared action cache as
documented in [docs/process/build-cache.md](docs/process/build-cache.md); no
BuildBuddy account is required to build or test Rue.

For IDE support, rust-analyzer reads the checked-in `rust-project.json` (Rue has
no Cargo workspace). Regenerate it with `./gen-rust-project.sh` when crates or
dependencies change — see [docs/development.md](docs/development.md#editor--ide-support).

## Reporting bugs and proposing changes

Open a **GitHub issue** for public bug reports, questions, and discussion.
Include a minimal `.rue` program that reproduces the problem and the command you
ran. For substantial language or architecture changes, open an issue to discuss
the design before writing code.

## Submitting a change

1. Fork the repository on GitHub and create a branch for your change.
2. Make your change with tests (see below), and format the tree:
   ```bash
   scripts/rue fmt
   scripts/rue test
   ```
3. Open a **GitHub pull request against the `trunk` branch** of
   `rue-language/rue`.

CI runs the same checks as `scripts/rue test` (which wraps `./test.sh`) and must
pass before a PR can merge, so running the full suite locally mirrors CI and is
the quickest way to avoid a bounce. New language features must be preview-gated
until complete.

Use a targeted spec, UI, or CLI test while iterating. Language behavior belongs
in specification tests; diagnostic presentation belongs in UI tests; driver,
ABI, multi-file, and runtime-I/O behavior belongs in CLI tests.

## How the maintainers work

Day-to-day, the maintainers use [Jujutsu (`jj`)](https://jj-vcs.github.io) for
local version control and [Linear](https://linear.app) (`RUE-NN`) for issue
tracking, and they mirror external GitHub issues into Linear. **None of this is
required to contribute** — ordinary Git and GitHub issues/PRs are fully
supported and are the intended path for external contributions. The internal
process (Linear access, agent/AI workflows, merge-queue management, and
repository setup) is documented for maintainers under
[docs/process/](docs/process/), which external contributors can safely ignore.

## Documentation

- [Development guide](docs/development.md)
- [Compiler architecture](docs/architecture.md)
- [Language overview](docs/language.md)
- [Language specification](docs/spec/)
- [Architecture decision records](docs/designs/)
- [Project processes (internal/maintainer-oriented)](docs/process/)
