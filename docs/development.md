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
scripts/rue premerge                 # required-CI test tier
scripts/rue slow                     # scheduled exhaustive tests
scripts/rue stress                   # opt-in resource stress tests
scripts/rue all                      # union of every test tier
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

The model is exactly one root source file per compile; additional files are
reached through `@import` and discovered transitively from the root. The legacy
flat-mode input form — extra `.rue` files listed positionally — was removed
(ADR-0046 / RUE-767): a second positional source is refused with a diagnostic
pointing at `@import` and `--source-manifest`.

Build-system integrations can constrain module reads with a source manifest:

```bash
printf 'main.rue\nhelper.rue\n' > sources.manifest
"$RUE" --source-manifest sources.manifest main.rue -o program
```

The manifest format is intentionally narrow: one source path per line, resolved
relative to the manifest file; blank lines and `#` comments are ignored. Entries
are allowed reads for `@import` resolution, not extra semantic roots.

Build-system integrations can also ask Rue what it actually read while
discovering the root import graph:

```bash
"$RUE" --emit deps main.rue
```

This prints a deterministic version-1 JSON envelope with a revision label and
three separately ordered components:

- `topology`: the compiler's canonical root and import outcomes (`resolved`,
  `missing`, or `ambiguous`), plus legal import cycles;
- `observations`: every compiler-generated candidate request and its physical or
  policy outcome; and
- `accepted_reads`: only regular source contents actually read and accepted,
  with their logical module, physical identity, metadata fingerprint, and
  content fingerprint.

The top-level `status` is `complete` for a closed valid graph. A structurally
closed graph containing a missing or ambiguous import is still emitted with
`status: "incomplete"`; Rue also renders the canonical diagnostics and exits
unsuccessfully. Parse, discovery-I/O, identity, and non-closure failures have no
canonical topology and therefore emit no dependency envelope. A denied request
is not an executed probe, an absent probe is not an accepted read, and the
source manifest remains the separately declared allowed-read set.

Run `"$RUE" --help` for targets, preview features, optimization levels,
logging, timing, manifests, and all emit stages.

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
| `crates/rue-allocator` | target-independent runtime heap policy |
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
treats an unexpected pass or fatal subprocess failure as a failure so obsolete
markers and infrastructure failures cannot linger.
Preview-language cases must enable their feature explicitly and do not count as
stable normative coverage.

## Tutorial snippets

Tutorial Rue fences are checked when their info string opts in:

- ````markdown
  ```rue check
  ```
  ```` compiles successfully.
- ````markdown
  ```rue compile-fail E0203
  ```
  ```` must fail compilation with the named diagnostic code, for intentionally
  invalid examples.
- ````markdown
  ```rue skip
  ```
  ```` is an explicit prose-only or context-dependent snippet.

Run the checker directly while editing tutorial chapters:

```bash
scripts/check-tutorial-snippets.py
```

Or run the Buck target used by CI-style validation:

```bash
./buck2 test //:tutorial-snippet-tests
```

Repository-wide quality gates are also Buck targets, so `./buck2 test //...`
includes spec traceability and ADR registry validation. To run them directly:

```bash
./buck2 test //:spec-traceability //:adr-registry-validation
```

During implementation, use the narrowest relevant command. Before submitting:

```bash
scripts/rue fmt
scripts/rue test
```

CI additionally runs Clippy, workflow linting, rust-project.json validation,
debug tests on Linux x86-64, Linux AArch64, and macOS, plus a focused
release-mode Linux smoke. The exhaustive release suite runs nightly and can be
dispatched manually. Fuzz, sanitizer, and website workflows run separately.

## Editor / IDE support

Rue builds with Buck2, not Cargo, so rust-analyzer is driven by a checked-in
`rust-project.json` rather than `Cargo.toml`. Point your editor's rust-analyzer
at the repository root and it will use that file directly.

Regenerate it from the Buck target graph whenever crates or dependencies change:

```bash
./gen-rust-project.sh                      # rewrites rust-project.json
python3 scripts/validate-rust-project.py   # checks it (also a CI gate)
```

The generator models every first-party crate plus the third-party crates they
depend on (following Buck alias/proc-macro indirection). The validator — run in
CI — fails if the checked-in file references a missing `root_module` or omits a
live first-party crate, so the model cannot silently drift out of date.

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
