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

## Corpus

The fuzzer uses a corpus of source files as seeds. A seed corpus can be automatically generated from the specification test files:

```bash
./buck2 run //crates/rue-fuzz:rue-fuzz -- --init-corpus crates/rue-fuzz/corpus
```

This extracts source code from all `.toml` test files in `crates/rue-spec/cases/`.

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

When a crash is detected, the crashing input is saved to the crash directory
(defaults to `crashes/` next to the corpus) as
`crash-<target>-<sighash>-<inputhash>.txt`. Crashes are **deduplicated by
signature** (panic message + location, or signal type), so one flooding bug
saves a single reproducer instead of thousands. To reproduce, feed the saved
input back to the target named in the filename:

```bash
# After finding a crash in, e.g., the parser target
./buck2 run //crates/rue:rue -- --emit tokens crashes/crash-parser-*.txt

# Or run the same fuzz target on a directory containing just that input
./buck2 run //crates/rue-fuzz:rue-fuzz -- parser <dir-with-the-file>
```

## Integration with CI

Fuzzing runs automatically in CI via `.github/workflows/fuzz.yml`. Each target runs for 5 minutes daily.

To run fuzzing locally for a limited time:

```bash
# Run each target for 5 minutes
for target in lexer parser sema compiler emitter emitter_sequence; do
    ./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate --max-time=300 $target crates/rue-fuzz/corpus
done
```

Any crash — panic or abort (signal) — causes a non-zero exit code, which fails
the CI job and uploads the saved reproducers as artifacts.

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
