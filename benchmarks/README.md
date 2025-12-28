# Rue Compiler Benchmarks

This directory contains benchmark programs for measuring Rue compiler performance.

## Structure

```
benchmarks/
├── manifest.toml       # Benchmark metadata
├── README.md           # This file
└── stress/             # Stress test programs
    ├── many_functions.rue    # 100+ functions
    ├── deep_nesting.rue      # Deeply nested blocks
    ├── large_structs.rue     # Many struct types
    ├── arithmetic_heavy.rue  # Expression-heavy code
    └── control_flow.rue      # Complex if/while/match
```

## Benchmark Descriptions

| Benchmark | What it tests | Size |
|-----------|--------------|------|
| `many_functions` | Function handling, symbol resolution | 100 functions |
| `deep_nesting` | Scope handling, block nesting | 10 nesting levels |
| `large_structs` | Type definitions, field access | 50 struct types |
| `arithmetic_heavy` | Expression parsing, codegen | ~100 expressions |
| `control_flow` | CFG construction | if/while/match mix |

## Running Benchmarks

### Using the Benchmark Runner (Recommended)

The `bench.sh` script handles the complete benchmark workflow:

```bash
# Run all benchmarks with defaults (5 iterations, append to history)
./bench.sh

# Custom number of iterations for more accuracy
./bench.sh --iterations 10

# Save to a specific file without updating history
./bench.sh --no-history --output results.json

# Show help
./bench.sh --help
```

The benchmark runner:
1. Builds the compiler in release mode
2. Parses `manifest.toml` to find benchmarks
3. Runs each benchmark multiple times
4. Calculates mean and standard deviation
5. Outputs JSON results
6. Appends to `website/static/benchmarks/history.json` (unless `--no-history`)

### Running Individual Benchmarks

For manual testing or debugging:

```bash
# Run a single benchmark with timing output
./buck2 run //crates/rue:rue -- --time-passes benchmarks/stress/many_functions.rue /tmp/out

# Get JSON timing output
./buck2 run //crates/rue:rue -- --benchmark-json benchmarks/stress/many_functions.rue /tmp/out
```

## Adding Benchmarks

1. Add a `.rue` file to `stress/` (or create a new category directory)
2. Add an entry to `manifest.toml`
3. Ensure the program compiles and runs correctly

Each benchmark should:
- Be large enough to produce measurable timing (aim for >1ms compilation)
- Focus on a specific compiler phase or feature
- Return a deterministic exit code for verification

## Benchmark History

Results are stored in `website/static/benchmarks/history.json` for the performance
dashboard. The history is limited to 100 most recent runs.

For more details on the performance tracking workflow, see `docs/perf-branch.md`.
