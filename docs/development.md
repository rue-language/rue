# Development guide

## Prerequisites

Install [Dotslash](https://dotslash-cli.com/). The repository uses it to
bootstrap Buck2; Buck2 downloads the pinned Rust toolchain and dependencies.
Cargo is not the build system.

Linux builds are otherwise self-contained. macOS needs Xcode Command Line
Tools because executable linking uses system tooling.

## Common commands

The `scripts/rue` wrapper is the supported entry point and can be invoked from
any directory inside the repository:

```bash
scripts/rue build                    # build the compiler
scripts/rue exec program.rue         # compile and run a program
scripts/rue quick                    # Rust unit tests
scripts/rue test [pattern]           # full suite, optionally filtered
scripts/rue spec 4.2                 # filtered specification cases
scripts/rue cli abi                  # filtered CLI integration cases
scripts/rue fmt                      # format first-party Rust
scripts/rue gc                       # remove stale Buck artifacts
```

For direct compiler invocations, resolve the binary through `scripts/rue-bin`:

```bash
RUE="$(scripts/rue-bin)"
"$RUE" main.rue -o program
"$RUE" --emit air --emit cfg main.rue
```

Multiple source files require `-o`. Run `"$RUE" --help` for targets, preview
features, optimization levels, logging, timing, and all emit stages.

Long-form Buck commands remain useful when working on build targets:

```bash
./buck2 build //crates/...
./buck2 test //crates/rue-codegen:rue-codegen-test
./buck2 run //crates/rue:rue -- source.rue -o program
```

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/rue` | CLI driver |
| `crates/rue-{lexer,parser,rir}` | parsing and untyped IR |
| `crates/rue-air` | semantic analysis and typed IR |
| `crates/rue-cfg` | control flow and target-independent optimization |
| `crates/rue-codegen` | x86-64 and AArch64 backends |
| `crates/rue-linker` | ELF/Mach-O objects and linking |
| `crates/rue-runtime` | target runtime support |
| `crates/rue-{spec,ui-tests,cli-tests}` | behavioral test harnesses |
| `crates/rue-{oracle,oracle-diff}` | independent evaluator and differential tests |
| `crates/rue-fuzz` | mutation/property fuzzing |
| `docs/spec/src` | authoritative language specification |
| `docs/designs` | architecture decision records |
| `website` | public website, tutorial, and field journal |

See [architecture.md](architecture.md) for the compiler pipeline.

## Choosing tests

- Add **unit tests** for local data structures, transformations, and invariants.
- Add **spec tests** for language syntax or semantics. Reference the relevant
  `chapter.section:paragraph` IDs; the traceability gate rejects dangling
  references and tracks normative coverage.
- Add **UI tests** for diagnostics, warnings, flags, and message presentation
  not mandated by the specification.
- Add **CLI tests** for the driver, filesystem/module loading, ABI behavior,
  multiple files, linking, runtime I/O, or internal-compiler-error regressions.

Known compiler defects use `known_bug = "RUE-NN"` in CLI cases. The harness
treats an unexpected pass as a failure so obsolete markers cannot linger.
Preview-language cases must enable their feature explicitly and do not count as
stable normative coverage.

During implementation, use the narrowest relevant command. Before submitting:

```bash
scripts/rue fmt
scripts/rue test
```

CI additionally runs Clippy, workflow linting, debug tests on Linux x86-64,
Linux AArch64, and macOS, plus a release-mode Linux suite. Fuzz, sanitizer,
website, and benchmark workflows run separately.

## Language and design changes

The specification, implementation, and tests form one change. New language
features require an ADR and a preview feature until their syntax, semantics,
diagnostics, and tests are complete. Follow [docs/designs/README.md](designs/README.md)
and the implementation process in [process/implementation.md](process/implementation.md).

Rue tracks work in Linear under `RUE-NN`; do not create repository TODO files
as a substitute for tracked issues.

## Version control

Rue uses [Jujutsu](https://jj-vcs.github.io/jj/latest/) for local version
control. This checkout may be a fork, so inspect remotes before publishing.

```bash
jj status
jj diff
jj describe -m "Concise change description"
jj new
```

Do not develop directly on `trunk`. The detailed commit and review workflow is
documented under [docs/process/](process/).
