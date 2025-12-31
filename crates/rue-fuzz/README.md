# Rue Fuzzer

Fuzz testing infrastructure for the Rue compiler. This crate helps find edge cases, crashes, and potential issues in the lexer, parser, and semantic analysis phases.

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
| `compiler` | Full frontend (through sema) | ~4,000-8,000 exec/s |

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

When a panic is detected, the crashing input is saved to the crash directory (defaults to `crashes/` next to the corpus). To reproduce:

```bash
# After finding a crash
./buck2 run //crates/rue:rue -- --emit tokens crashes/crash-*.txt

# Or compile it
./buck2 run //crates/rue:rue -- crashes/crash-*.txt output
```

## Integration with CI

To add fuzzing to CI, run the fuzzer for a limited time:

```bash
# Run each target for 5 minutes
for target in lexer parser compiler; do
    ./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate --max-time=300 $target crates/rue-fuzz/corpus
done
```

Any panics will cause a non-zero exit code.

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
- Compiler frontend never panics on valid or invalid programs
- All components handle arbitrary strings without panicking

## Design

The fuzzer is designed to work with Buck2 without requiring cargo-fuzz or libFuzzer. It:

1. Loads inputs from a corpus directory
2. Optionally mutates inputs (byte-level mutations)
3. Runs the fuzz target in a panic-catching wrapper
4. Saves any crashing inputs

Additionally, proptest-based generators create syntactically valid programs for deeper testing.

Each fuzz target exercises a specific phase of the compiler:
- **Lexer**: Should never panic, always return tokens or an error
- **Parser**: Should never panic, always return an AST or an error
- **Compiler**: Should never panic, always compile or return errors
