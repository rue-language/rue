# Rue Fuzzer

Fuzz testing infrastructure for the Rue compiler. This crate helps find edge cases, crashes, and potential issues in the lexer, parser, semantic analysis, and code generation phases.

## Quick Start

```bash
# Create a seed corpus from existing test files
./buck2 run //crates/rue-fuzz:rue-fuzz -- --init-corpus crates/rue-fuzz/corpus

# Run the lexer fuzzer
./buck2 run //crates/rue-fuzz:rue-fuzz -- lexer crates/rue-fuzz/corpus

# Run with mutations for better coverage
./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate parser crates/rue-fuzz/corpus

# Run for a specific duration
./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate --max-time=300 compiler crates/rue-fuzz/corpus
```

## Fuzz Targets

| Target | Description | Speed |
|--------|-------------|-------|
| `lexer` | Tokenization only | ~27,000 exec/s |
| `parser` | Lexing + parsing | ~6,500 exec/s |
| `sema` | Semantic analysis (type checking, inference) | ~4,000-8,000 exec/s |
| `compiler` | Full frontend (through sema) | ~4,000-8,000 exec/s |
| `warm_session` | Bounded retained-session edit sequences with warm/fresh parity | bounded |
| `payload_schemas` | Production RIR/AIR/CFG payload validation path | ~4,000-8,000 exec/s |
| `emitter` | x86-64 instruction encoding | ~15,000 exec/s |
| `emitter_sequence` | Instruction sequences with labels/jumps | ~10,000 exec/s |

## Options

| Option | Description |
|--------|-------------|
| `--list` | List available fuzz targets |
| `--init-corpus <dir>` | Create seed corpus from test files |
| `--mutate` | Enable input mutation |
| `--max-time=<secs>` | Maximum time to run |
| `--max-runs=<n>` | Maximum number of runs |
| `--crash-dir=<dir>` | Directory to save crashes |
| `--print-interval=<n>` | Print progress every N runs |
| `--per-input-timeout=<secs>` | Kill and report inputs running longer than this (default 5) |
| `--seed=<n>` | Mutation RNG seed; defaults to a per-run value, printed at start for replay |
| `--evolve-corpus=<dir>` | Save bounded, content-addressed successful mutations for a later run |
| `--prepare-corpus=<dir> --fresh-corpus=<dir> --input-corpus=<dir> --output-corpus=<dir>` | Validate restored cache bytes and build separate fresh-input and evolved-output trees |
| `--publish-corpus=<dir> --cache-corpus=<dir>` | Copy the clean bounded tree into the same-path cache generation |

## Corpus

The fuzzer uses a corpus of source files as seeds. A seed corpus can be automatically generated from the specification test files:

```bash
./buck2 run //crates/rue-fuzz:rue-fuzz -- --init-corpus crates/rue-fuzz/corpus
```

This extracts source code from all `.toml` test files in `crates/rue-spec/cases/`.

Nightly CI restores evolved bytes into a private directory for each target and
merges fresh spec seeds before every five-minute campaign. Only successful
mutations are retained, with a bounded content-addressed file set; crashes are
written only to the redacted crash-artifact path. Cache-restored files are
treated as untrusted bytes: they are never executed as scripts, followed as
symlinks, or printed as input contents. The fuzzer prints its randomly chosen
campaign seed, so a run can be replayed with `--seed=<n>`.

## Mutation Strategies

When `--mutate` is enabled, the fuzzer applies these mutations to corpus inputs:

- Bit flips
- Byte flips
- Byte insertion/deletion
- Arithmetic modifications
- Keyword splicing (inserts Rue keywords)
- Chunk shuffling and duplication

## Finding Bugs

### Crash detection: panics *and* aborts (RUE-43)

Each input is executed in a **forked child process** (see `src/harness.rs`).
The parent inspects the child's `waitpid` status, so the fuzzer detects not
only Rust panics but also *aborts* that tear the process down without
unwinding — stack overflow (SIGSEGV on the guard page), OOM/`abort()`
(SIGABRT), and SIGSEGV/SIGFPE from unsafe code. The previous in-process
`catch_unwind` harness was blind to all of these (and, because this toolchain
builds with `panic = abort`, it could not actually catch panics either — they
abort). The child streams the panic message over a pipe from its panic hook so
panics keep a source-location dedup signature even under `panic = abort`.

Two further failure classes are covered:

- **Hangs**: each input has a wall-clock budget (`--per-input-timeout`,
  default 5s). A child still running at the deadline is SIGKILLed and
  reported as a `timeout` crash — infinite loops and superlinear blowups
  are compiler bugs too, and without the deadline a single hung input would
  wedge the whole run in `waitpid`.
- **Graceful ICEs**: the sema/compiler targets inspect the returned errors
  and panic on `ErrorKind::InternalError` — an `ice_error!` that returns a
  normal `Err` is still a compiler bug, even though it doesn't crash.

When a crash is detected, the crashing input is saved to the crash directory
(defaults to `crashes/` next to the corpus) as
`crash-<target>-<sighash>-<inputhash>.txt`, with a `.meta` sibling recording
the target, signature, and crash description so downloaded CI artifacts are
self-describing. Crashes are **deduplicated by signature** (panic message + location, or signal type), so one flooding bug
saves a single reproducer instead of thousands. To reproduce, feed the saved
input back to the target named in the filename:

```bash
# After finding a crash in, e.g., the parser target
./buck2 run //crates/rue:rue -- --emit tokens crashes/crash-parser-*.txt

# Or run the same fuzz target on a directory containing just that input
./buck2 run //crates/rue-fuzz:rue-fuzz -- parser <dir-with-the-file>
```

## Differential fuzzing (oracle vs compiled) — RUE-247

A separate, complementary fuzzer lives in `crates/rue-oracle-diff`: it generates
random **valid, well-typed** programs (in the subset the `rue-oracle` reference
interpreter models) and runs each through *both* the oracle and the real
compiler + native binary, comparing exit code and `@dbg` stdout. A disagreement
is an automatically-discovered **miscompile** with a deterministic, seed-based
repro (not a crash — a *wrong answer*). Because the generator promises valid
programs inside the oracle's modeled subset, compiler rejection and
`Unsupported` are also fail-closed generator-contract findings with the same
seed/source repro format. Repros land in this crate's `crashes/` directory as
`oracle-diff-seed-<seed>.rue` and are uploaded by the same CI artifact step.

```bash
RUE_BINARY="$(scripts/rue-bin)" ./buck2 run //crates/rue-oracle-diff:rue-oracle-diff -- \
    fuzz --seeds 500                 # cross-check 500 generated programs
./buck2 run //crates/rue-oracle-diff:rue-oracle-diff -- dump 42   # inspect seed 42's program
```

## Integration with CI

Fuzzing runs automatically in CI via `.github/workflows/fuzz.yml`. Each target runs for 5 minutes daily; the differential fuzzer (above) runs a bounded 500-seed batch in the same workflow.

To run fuzzing locally for a limited time:

```bash
# Run each target for 5 minutes
for target in lexer parser sema compiler warm_session payload_schemas emitter emitter_sequence; do
    ./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate --max-time=300 $target crates/rue-fuzz/corpus
done
```

Any crash — panic or abort (signal) — causes a non-zero exit code, which fails
the CI job and uploads the saved reproducers as artifacts.

A failed nightly run then reports every crash it found to the issue tracker via
`scripts/fuzz-report-failure.py`: **one issue per distinct crash fingerprint**
in the Rue Linear team, deduplicated against open issues so a recurrence becomes
a comment rather than a second issue. The fingerprint is derived from the target
plus the *normalized* signature recorded in the `.meta` sibling (addresses, temp
paths, source line numbers, and generator seeds erased), which is what lets the
same bug dedup across nights. See
[docs/process/fuzz-failure-reporting.md](../../docs/process/fuzz-failure-reporting.md)
— including the one-time `LINEAR_API_KEY` secret setup, without which the
workflow falls back to filing GitHub Issues.

## Proptest Integration

The fuzzer includes proptest-based tests that generate syntactically valid Rue programs. These run as part of the unit tests:

```bash
./buck2 test //crates/rue-fuzz:rue-fuzz-test
```

The proptest generators (`src/generators.rs`) can create:
- Valid identifiers (avoiding keywords)
- Primitive types (i8, i16, i32, i64, u8, u16, u32, u64, bool)
- Expressions (literals, binary ops, unary ops, if/else, blocks)
- Statements (let, assignment, return)
- Functions and struct/enum definitions
- Complete programs with main functions

This enables much more effective testing than random byte mutation, as it generates inputs that exercise deeper parts of the compiler (semantic analysis, type checking).

The proptest tests verify:
- Lexer never panics on any generated expression or program
- Parser never panics on any generated program
- Sema never panics on valid or invalid programs (type inference, name resolution)
- Compiler frontend never panics on valid or invalid programs
- All components handle arbitrary strings without panicking

### Codegen Generators

The fuzzer also includes specialized generators for the code generation phase (`src/codegen_generators.rs`):
- Physical and virtual register operands
- x86-64 instructions with various register combinations
- Instruction sequences with labels and jumps
- Immediate values (boundary cases like i32::MIN, i32::MAX)
- Shift amounts and stack offsets

These enable testing the instruction emitter with structured inputs that exercise:
- REX prefix encoding with unusual register combinations
- Immediate value encoding edge cases
- Label resolution and jump fixups
- Various instruction encodings

## Design

The fuzzer is designed to work with Buck2 without requiring cargo-fuzz or libFuzzer. It:

1. Loads inputs from a corpus directory
2. Optionally mutates inputs (byte-level mutations)
3. Runs each input in a forked child process, detecting panics *and* aborts
   (signal deaths: SIGSEGV/SIGABRT/SIGFPE) via the child's wait-status
4. Saves a deduplicated reproducer for each distinct crash signature

Additionally, proptest-based generators create syntactically valid programs and structured codegen inputs for deeper testing.

Each fuzz target exercises a specific phase of the compiler:
- **Lexer**: Should never panic, always return tokens or an error
- **Parser**: Should never panic, always return an AST or an error
- **Sema**: Should never panic, always type-check or return errors (tests assumptions about RIR validity)
- **Compiler**: Should never panic, always compile or return errors
- **Emitter**: Should never panic on any valid instruction sequence
- **Emitter Sequence**: Should handle labels and jumps without panicking
